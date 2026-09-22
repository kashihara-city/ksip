// Post-SIP test tools: bounded asynchronous receive-only WAV recording.
#include <cmath>
#include <re.h>
#include <rem.h>
#include <baresip.h>
#include <array>
#include <atomic>
#include <condition_variable>
#include <cstdio>
#include <cstring>
#include <filesystem>
#include <memory>
#include <mutex>
#include <thread>
#include <algorithm>
#include <unordered_map>
#include <string>

namespace {
struct Recorder {
    std::mutex mutex;
    std::condition_variable wake;
    std::array<int16_t, 48000 * 10> ring{};
    size_t rd=0, wr=0, count=0;
    uint32_t rate=0, channels=0;
    uint64_t bytes=0, dropped=0;
    bool done=false, failed=false;
    FILE *file=nullptr;
    std::thread worker;
    static void le16(FILE *f, uint16_t n) { fputc(n & 255,f); fputc(n >> 8,f); }
    static void le32(FILE *f, uint32_t n) { le16(f,n & 65535); le16(f,n >> 16); }
    void header() {
        fseek(file,0,SEEK_SET); fwrite("RIFF",1,4,file); le32(file,36+(uint32_t)bytes);
        fwrite("WAVEfmt ",1,8,file); le32(file,16); le16(file,1); le16(file,(uint16_t)channels);
        le32(file,rate); le32(file,rate*channels*2); le16(file,(uint16_t)(channels*2)); le16(file,16);
        fwrite("data",1,4,file); le32(file,(uint32_t)bytes);
    }
    explicit Recorder(const char *path, uint32_t sr, uint32_t ch):rate(sr),channels(ch) {
        file=_wfopen(std::filesystem::u8path(path).c_str(),L"wb");
        if (!file) return;
        header();
        worker=std::thread([this] {
            std::array<int16_t,8192> buf;
            for (;;) {
                size_t n;
                { std::unique_lock<std::mutex> lock(mutex);
                  wake.wait(lock,[this]{return done || count;});
                  if (!count && done) break;
                  n=std::min(count,buf.size());
                  for(size_t i=0;i<n;++i) {buf[i]=ring[rd];rd=(rd+1)%ring.size();}
                  count-=n;
                }
                if(bytes+n*2>0xffff0000ULL || fwrite(buf.data(),2,n,file)!=n) {failed=true;break;}
                bytes+=n*2;
            }
            header(); if(ferror(file) || fflush(file)!=0) failed=true;
        });
    }
    bool finish() {
        {std::lock_guard<std::mutex> lock(mutex);done=true;}
        wake.notify_one();
        if(worker.joinable())worker.join();
        if(file) {if(fclose(file)!=0)failed=true;file=nullptr;info("postlab: receive WAV closed (%llu bytes, %llu dropped samples, error=%d)\n",bytes,dropped,failed);}
        return !failed && !dropped;
    }
    ~Recorder() {finish();}
    void push(const auframe *f) {
        std::lock_guard<std::mutex> lock(mutex);
        if(!file || done)return;
        for(size_t i=0;i<f->sampc;++i) {
            if(count==ring.size()) {dropped+=f->sampc-i;break;}
            int16_t sample;
            if(f->fmt==AUFMT_S16LE) sample=((int16_t*)f->sampv)[i];
            else if(f->fmt==AUFMT_FLOAT) sample=(int16_t)std::clamp(((float*)f->sampv)[i]*32768.f,-32768.f,32767.f);
            else return;
            ring[wr]=sample;wr=(wr+1)%ring.size();++count;
        }
        wake.notify_one();
    }
};
std::mutex gate;
std::unique_ptr<Recorder> recording;
std::atomic<float> microphone_gain{1.f};
std::atomic<float> speaker_gain{1.f};
struct Encode {aufilt_enc_st base;};
struct Decode {aufilt_dec_st base; const audio *stream; uint32_t rate,channels;};
std::unordered_map<const audio*,Decode*> decoders;
const audio *recording_audio=nullptr;
// A call reports ESTABLISHED before its decode filter exists, so a requested
// target is held here and bound as soon as the filter appears.
std::string reserved_call, reserved_path;
bool begin_recording(const std::string &path,uint32_t rate,uint32_t channels,const audio *stream) {
    try {
        auto r=std::make_unique<Recorder>(path.c_str(),rate,channels);
        if(!r->file)return false;
        recording=std::move(r);
    } catch(...) {return false;}
    recording_audio=stream;return true;
}
void destroy(void *p) {
    auto d=static_cast<Decode*>(p);list_unlink(&d->base.le);
    std::lock_guard<std::mutex> lock(gate);
    decoders.erase(d->stream);
    // Keep the WAV session alive when a call ends. Rust may select the other
    // line next, so destroying one decoder must not split the recording.
    if(recording_audio==d->stream)recording_audio=nullptr;
}
void destroy_encode(void *p) {auto e=static_cast<Encode*>(p);list_unlink(&e->base.le);}
int update_encode(aufilt_enc_st **st, void**, const aufilt*, aufilt_prm*, const audio*) {
    if(*st)return 0;
    auto e=(Encode*)mem_zalloc(sizeof(Encode),destroy_encode);
    if(!e)return ENOMEM;
    *st=&e->base;return 0;
}
void amplify(auframe *f,float gain) {
    if(gain<=1.f)return;
    if(f->fmt==AUFMT_S16LE) {
        auto samples=static_cast<int16_t*>(f->sampv);
        for(size_t i=0;i<f->sampc;++i)samples[i]=(int16_t)std::clamp(std::lrint(samples[i]*gain),-32768L,32767L);
    }
    else if(f->fmt==AUFMT_FLOAT) {
        auto samples=static_cast<float*>(f->sampv);
        for(size_t i=0;i<f->sampc;++i)samples[i]=std::clamp(samples[i]*gain,-1.f,1.f);
    }
}
int process_encode(aufilt_enc_st*,auframe *f) {amplify(f,microphone_gain.load(std::memory_order_relaxed));return 0;}
int update_decode(aufilt_dec_st **st, void**, const aufilt*, aufilt_prm *p, const audio *stream) {
    if(*st)return 0;
    auto d=(Decode*)mem_zalloc(sizeof(Decode),destroy);
    if(!d)return ENOMEM;
    {
        std::lock_guard<std::mutex> lock(gate);
        d->stream=stream;d->rate=p->srate;d->channels=p->ch;decoders[stream]=d;
        if(!reserved_call.empty()) {
            auto c=uag_call_find(reserved_call.c_str());
            if(c && call_audio(c)==stream) {
                bool ok=reserved_path.empty() ? (recording_audio=stream,true)
                                              : begin_recording(reserved_path,p->srate,p->ch,stream);
                if(ok)info("postlab: receive recording bound to the reserved call\n");
                else warning("postlab: cannot open the reserved WAV file\n");
                reserved_call.clear();reserved_path.clear();
            }
        }
    }
    *st=&d->base; return 0;
}
int process_decode(aufilt_dec_st *st,auframe *f) {
    {std::lock_guard<std::mutex> lock(gate);if(recording && recording_audio==reinterpret_cast<Decode*>(st)->stream)recording->push(f);}
    amplify(f,speaker_gain.load(std::memory_order_relaxed));return 0;
}
int start(re_printf *pf, void *arg) {
    auto a=(cmd_arg*)arg;
    if(!a || !str_isset(a->prm))return EINVAL;
    std::string text=a->prm;auto space=text.find(' ');
    const audio *target=nullptr;std::string path=text,id;
    if(space!=std::string::npos) {
        id=text.substr(0,space);
        auto c=uag_call_find(id.c_str());
        if(c){target=call_audio(c);path=text.substr(space+1);}
        else id.clear();
    }
    std::lock_guard<std::mutex> lock(gate);
    if(recording || !reserved_path.empty())return EALREADY;
    if(!target && decoders.size()==1)target=decoders.begin()->first;
    auto found=decoders.find(target);
    if(found==decoders.end()) {
        if(id.empty())return re_hprintf(pf,"Call audio is not active\n"), EAGAIN;
        reserved_call=id;reserved_path=path;
        return re_hprintf(pf,"Receive-only recording reserved\n");
    }
    if(!begin_recording(path,found->second->rate,found->second->channels,target))
        return re_hprintf(pf,"Cannot open WAV file\n"), EIO;
    return re_hprintf(pf,"Receive-only recording started\n");
}
int stop(re_printf *pf,void*) {
    std::lock_guard<std::mutex> lock(gate);
    reserved_call.clear();reserved_path.clear();
    if(recording && !recording->finish()) {
        recording.reset();re_hprintf(pf,"WAV write failed or samples were dropped; recording is incomplete\n");return EIO;
    }
    recording.reset();recording_audio=nullptr;return re_hprintf(pf,"Receive-only recording stopped\n");
}
int select_recording(re_printf *pf,void *arg) {
    auto a=(cmd_arg*)arg;
    if(!a || !str_isset(a->prm))return EINVAL;
    std::lock_guard<std::mutex> lock(gate);
    if(!recording && reserved_path.empty())return ENOENT;
    if(strcmp(a->prm,"-")==0) {
        recording_audio=nullptr;reserved_call.clear();
        return re_hprintf(pf,"Receive recording input paused\n");
    }
    auto c=uag_call_find(a->prm);
    if(!c)return ENOENT;
    auto target=call_audio(c);
    auto found=decoders.find(target);
    if(found==decoders.end()) {
        // The call exists but is not carrying audio yet; bind it once it does.
        recording_audio=nullptr;reserved_call=a->prm;
        return re_hprintf(pf,"Receive recording input reserved\n");
    }
    if(!reserved_path.empty()) {
        if(!begin_recording(reserved_path,found->second->rate,found->second->channels,target))
            return re_hprintf(pf,"Cannot open WAV file\n"), EIO;
        reserved_path.clear();
    }
    else recording_audio=target;
    reserved_call.clear();
    return re_hprintf(pf,"Receive recording input switched\n");
}
int gain(re_printf *pf,void *arg) {
    auto a=(cmd_arg*)arg;
    if(!a || !str_isset(a->prm))return EINVAL;
    char kind[16]={};unsigned level=0;char extra=0;
    if(sscanf(a->prm,"%15s %u %c",kind,&level,&extra)!=2 || level<100 || level>200)return EINVAL;
    auto value=level/100.f;
    if(strcmp(kind,"microphone")==0)microphone_gain.store(value,std::memory_order_relaxed);
    else if(strcmp(kind,"speaker")==0)speaker_gain.store(value,std::memory_order_relaxed);
    else return EINVAL;
    return re_hprintf(pf,"%s software gain %u%%\n",kind,level);
}
aufilt filter={};
const cmd commands[]={
    {"lab_record",0,CMD_PRM,"Record receive-only WAV",start},
    {"lab_record_select",0,CMD_PRM,"Select the call appended to the current WAV",select_recording},
    {"lab_stop",0,0,"Finish receive WAV",stop},
    {"lab_gain",0,CMD_PRM,"Set microphone/speaker software gain",gain},
};
int init(){
    uint32_t mic=100,speaker=100;
    (void)conf_get_u32(conf_cur(),"ksip_microphone_gain",&mic);
    (void)conf_get_u32(conf_cur(),"ksip_speaker_gain",&speaker);
    microphone_gain.store(std::clamp(mic,100u,200u)/100.f);
    speaker_gain.store(std::clamp(speaker,100u,200u)/100.f);
    filter.name="postlab";filter.encupdh=update_encode;filter.ench=process_encode;
    filter.decupdh=update_decode;filter.dech=process_decode;
    aufilt_register(baresip_aufiltl(),&filter);return cmd_register(baresip_commands(),commands,RE_ARRAY_SIZE(commands));
}
int close(){cmd_unregister(baresip_commands(),commands);aufilt_unregister(&filter);std::lock_guard<std::mutex> lock(gate);recording.reset();return 0;}
}
extern "C" const struct mod_export DECL_EXPORTS(postlab)={"postlab","aufilt",init,close};
