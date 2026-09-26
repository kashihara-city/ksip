// Account credentials stay in Windows Credential Manager and process memory.
#define WIN32_LEAN_AND_MEAN
#include <windows.h>
#include <wincred.h>
#include <cmath>
#include <re.h>
#include <rem.h>
#include <baresip.h>
#include <string>
#include <array>
#include <unordered_map>
#include <vector>
#include <cstring>
#include <cctype>
#include "ksip_audio_bridge.h"

namespace {
ua *account_ua = nullptr; // Owned by the UA group, not this module.
std::string authority, sip_scheme="udp", registration="UNCONFIGURED", original, consultation, outcome;
// The call this module ends itself once a transfer has gone through. Its
// closing is part of the transfer, not a call the person hung up.
std::string transfer_hangup;
std::string registered_transport, media_encryption;
bool pending=false;
tmr transfer_timer;
// A transfer sends its REFER only once the holds it sent have been answered:
// the timer fires it, and these are the calls whose answer is still waited for.
tmr refer_timer;
std::vector<std::string> refer_waiting;
bool refer_armed=false;
// Retries the park subscriptions a while after one closes.
tmr parking_timer;
int subscribe_parking();
int subscribe_mwi();
struct ParkSlot { struct sipsub *sub=nullptr; std::string number; std::string state="UNKNOWN"; };
// The numbers the buttons watch: up to thirty dialog subscriptions, six
// for the phone and the rest for the panel beside it.
std::array<ParkSlot,30> parking;
bool sip_message_log=false;
uint32_t register_interval=300;
std::unordered_map<std::string,std::string> connected_identity;
// The name a P-Asserted-Identity carried for a call, when it carried one.
std::unordered_map<std::string,std::string> connected_name;
// A display name as the window shows it: without the quotes a header puts
// around it, and empty when there is none.
std::string display_name(const pl &name) {
    if (!pl_isset(&name)) return "";
    std::string s(name.p,name.l);
    while (!s.empty() && (s.back()==' ' || s.back()=='\t')) s.pop_back();
    if (s.size()>=2 && s.front()=='"' && s.back()=='"') s=s.substr(1,s.size()-2);
    return s;
}
call *find(const std::string &id) { return id.empty() ? nullptr : uag_call_find(id.c_str()); }
// The outcome is counted as well as named: the window shows a notice each
// time one is set, and the same outcome twice in a row (returning to the held
// call after each of two second calls) would otherwise look unchanged to it.
unsigned outcome_seq=0;
void set_outcome(const char *code) { outcome=code; ++outcome_seq; }
// Puts every call being talked on, other than the given one, on hold. The
// module does this itself (call_hold_other_calls is off): baresip would also
// do it when a second call is answered by the far end, and that took the
// person away from the call they had gone back to.
int hold_others(call *except) {
    int err=0;
    for (le *u=list_head(uag_list());u;u=u->next)
        for (le *l=list_head(ua_calls(static_cast<ua*>(u->data)));l;l=l->next) {
            auto other=static_cast<call*>(l->data);
            if (other!=except && call_state(other)==CALL_STATE_ESTABLISHED && !call_is_onhold(other)) err|=call_hold(other,true);
        }
    return err;
}
// Whether some other call is being talked on right now.
bool talking_elsewhere(call *except) {
    for (le *u=list_head(uag_list());u;u=u->next)
        for (le *l=list_head(ua_calls(static_cast<ua*>(u->data)));l;l=l->next) {
            auto other=static_cast<call*>(l->data);
            if (other!=except && call_state(other)==CALL_STATE_ESTABLISHED && !call_is_onhold(other)) return true;
        }
    return false;
}
bool transfer_reversed=false;
// Set when this phone was taken off the server on purpose. The 200 OK for a
// de-registration arrives as a register event, which must not undo it.
bool unregistered=false;
// Do not disturb: an incoming call is answered with 486 Busy Here, so the PBX
// treats the phone as busy rather than absent. Never kept across a start.
bool dnd=false;
// The voicemail box's message-summary subscription, and the last summary
// body the server sent ("Messages-Waiting: yes", "Voice-Message: 2/5").
// The app reads the counts out of it.
struct sipsub *mwi_sub=nullptr;
std::string mwi_summary, own_user;
void clear_transfer() { original.clear(); consultation.clear(); pending=false; transfer_reversed=false; tmr_cancel(&transfer_timer); refer_armed=false; refer_waiting.clear(); tmr_cancel(&refer_timer); }
// A button may name a full SIP URI instead of a number; it is passed on as it
// is, and only has to be one line of visible ASCII.
bool sip_uri(const std::string &s) {
    std::string head=s.substr(0,5);for(auto &ch:head)ch=static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
    return head.rfind("sip:",0)==0 || head.rfind("sips:",0)==0;
}
bool address_ok(const std::string &s) {
    if(s.empty() || s.size()>200)return false;
    for(unsigned char ch:s)if(ch<0x21 || ch>0x7e)return false;
    return true;
}
bool token(const char *s, const char *extra) {
    if (!s || !*s || strlen(s)>253) return false;
    for (;*s;++s) if (!(static_cast<unsigned char>(*s)<128 && (isalnum(static_cast<unsigned char>(*s)) || strchr(extra,*s)))) return false;
    return true;
}
std::string sip_header(const uint8_t *packet,size_t length,const char *name) {
    const size_t name_length=strlen(name);size_t pos=0;
    while(pos<length) {
        size_t end=pos;while(end+1<length && !(packet[end]=='\r' && packet[end+1]=='\n'))++end;
        if(end==pos)break;
        if(end-pos>name_length && packet[pos+name_length]==':') {
            bool match=true;for(size_t i=0;i<name_length;++i)
                if(std::tolower(packet[pos+i])!=std::tolower(static_cast<unsigned char>(name[i])))match=false;
            if(match) {
                size_t value=pos+name_length+1;while(value<end && (packet[value]==' ' || packet[value]=='\t'))++value;
                while(end>value && (packet[end-1]==' ' || packet[end-1]=='\t'))--end;
                return std::string(reinterpret_cast<const char*>(packet+value),end-value);
            }
        }
        if(end+1>=length)break;pos=end+2;
    }
    return {};
}
void log_sip_message(bool tx,const uint8_t *packet,size_t length) {
    // Secrets never reach the log: digest headers are replaced before printing,
    // together with any folded continuation lines of theirs (RFC 3261 7.3.1),
    // and so are the SRTP keys an SDP body carries in a=crypto (RFC 4568),
    // which SDES and OSRTP put in the signalling. The file this goes to
    // outlives the call, and a key in it would unlock a captured recording.
    static const char *secrets[]={"authorization","proxy-authorization","www-authenticate","proxy-authenticate"};
    std::string text(reinterpret_cast<const char*>(packet),length);
    bool hiding=false;
    for(size_t pos=0;pos<text.size();) {
        size_t end=text.find("\r\n",pos);
        if(end==std::string::npos)end=text.size();
        std::string line=text.substr(pos,end-pos);
        pos=end+2;
        if(!line.empty() && (line[0]==' ' || line[0]=='\t')) {
            if(hiding)continue;
        }
        else hiding=false;
        auto colon=line.find(':');
        if(colon!=std::string::npos) {
            std::string name=line.substr(0,colon);
            for(auto &ch:name)ch=static_cast<char>(std::tolower(static_cast<unsigned char>(ch)));
            for(auto secret:secrets)if(name==secret){line=line.substr(0,colon)+": ***";hiding=true;break;}
        }
        if(line.compare(0,9,"a=crypto:")==0) {
            auto key=line.find(" inline:");
            if(key!=std::string::npos)line=line.substr(0,key)+" inline:***";
        }
        if(!line.empty())info("ksip sip %s %s\n",tx?">":"<",line.c_str());
    }
}
void sip_trace(bool tx,enum sip_transp tp,const sa*,const sa*,const uint8_t *packet,size_t length,void*) {
    if(sip_message_log && packet && length)log_sip_message(tx,packet,length);
    if(tx || !packet || length<7)return;
    // The register events carry no SIP message, so the transport that actually
    // carried the registration is read from the answer itself.
    if(!unregistered && length>=9 && memcmp(packet,"SIP/2.0 2",9)==0 &&
       sip_header(packet,length,"CSeq").find("REGISTER")!=std::string::npos)
        registered_transport=sip_transp_name(tp);
    bool update=length>=7 && memcmp(packet,"UPDATE ",7)==0;
    bool reinvite=length>=7 && memcmp(packet,"INVITE ",7)==0;
    if(!update && !reinvite)return;
    auto callid=sip_header(packet,length,"Call-ID"),pai=sip_header(packet,length,"P-Asserted-Identity");
    if(callid.empty() || pai.empty() || !find(callid))return;
    pl value{pai.data(),pai.size()};sip_addr address{};
    if(sip_addr_decode(&address,&value) || !pl_isset(&address.uri.user))return;
    std::string uri="sip:"+std::string(address.uri.user.p,address.uri.user.l);
    if(pl_isset(&address.uri.host))uri+="@"+std::string(address.uri.host.p,address.uri.host.l);
    connected_identity[callid]=uri;
    if(pl_isset(&address.dname))connected_name[callid]=display_name(address.dname);
}
int auth_handler(char **username,char **password,const char *realm,void *arg) {
    return account_auth(static_cast<account*>(arg),username,password,realm);
}
void parking_notify(struct sip *sip,const struct sip_msg *msg,void *arg) {
    auto slot=static_cast<ParkSlot*>(arg);
    std::string body(reinterpret_cast<const char*>(mbuf_buf(msg->mb)),mbuf_get_left(msg->mb));
    // Every dialog state but terminated counts as in use (RFC 4235).
    bool active=body.find("<state>confirmed</state>")!=std::string::npos ||
        body.find("<state>early</state>")!=std::string::npos ||
        body.find("<state>proceeding</state>")!=std::string::npos ||
        body.find("<state>trying</state>")!=std::string::npos;
    slot->state=active ? "INUSE" : "IDLE";
    (void)sip_treply(nullptr,sip,msg,200,"OK");
}
void parking_retry(void*) {
    int err=subscribe_parking();
    if(subscribe_mwi())err=EAGAIN;
    if(err)tmr_start(&parking_timer,30000,parking_retry,nullptr);
}
// A subscription the server ends or refuses is written down with its answer:
// it explains buttons that stay unknown, and a quit that waits on the server.
void subscription_closed(const char *what,int err,const struct sip_msg *msg) {
    if(msg)info("ksip: %s subscription closed by %u %r\n",what,static_cast<unsigned>(msg->scode),&msg->reason);
    else info("ksip: %s subscription closed (%m)\n",what,err);
}
void parking_closed(int err,const struct sip_msg *msg,const struct sipevent_substate*,void *arg) {
    // The subscription is over, so its reference goes here, as baresip's own
    // presence module does. The server is asked again after a while rather
    // than only at the next registration, or the buttons would stay unknown
    // for minutes.
    auto slot=static_cast<ParkSlot*>(arg);subscription_closed(("parking "+slot->number).c_str(),err,msg);
    slot->sub=static_cast<sipsub*>(mem_deref(slot->sub));slot->state="UNKNOWN";
    tmr_start(&parking_timer,30000,parking_retry,nullptr);
}
void mwi_notify(struct sip *sip,const struct sip_msg *msg,void *) {
    mwi_summary=std::string(reinterpret_cast<const char*>(mbuf_buf(msg->mb)),mbuf_get_left(msg->mb));
    // The first notify is the proof that the server took the subscription.
    info("ksip: mwi notify, %zu bytes\n",mwi_summary.size());
    (void)sip_treply(nullptr,sip,msg,200,"OK");
}
void parking_retry(void*);
void mwi_closed(int err,const struct sip_msg *msg,const struct sipevent_substate*,void*) {
    subscription_closed("mwi",err,msg);
    mwi_sub=static_cast<sipsub*>(mem_deref(mwi_sub));mwi_summary.clear();
    tmr_start(&parking_timer,30000,parking_retry,nullptr);
}
int subscribe_mwi() {
    if(!account_ua || !ua_isregistered(account_ua) || mwi_sub || own_user.empty())return 0;
    const char *routev[1]={ua_outbound(account_ua)};
    std::string uri="sip:"+own_user+"@"+authority+";transport="+sip_scheme;
    int err=sipevent_subscribe(&mwi_sub,uag_sipevent_sock(),uri.c_str(),nullptr,
        account_aor(ua_account(account_ua)),"message-summary",nullptr,600,ua_cuser(account_ua),
        routev,routev[0] ? 1 : 0,auth_handler,ua_account(account_ua),true,nullptr,
        mwi_notify,mwi_closed,nullptr,"Accept: application/simple-message-summary\r\n");
    if(err)warning("ksip: mwi subscription to %s failed (%d)\n",uri.c_str(),err);
    else info("ksip: mwi subscription to %s\n",uri.c_str());
    return err;
}
void clear_parking_subscriptions() {
    tmr_cancel(&parking_timer);
    if(mwi_sub){auto sub=mwi_sub;mwi_sub=nullptr;mem_deref(sub);}
    mwi_summary.clear();
    for(auto &slot:parking) {
        auto sub=slot.sub;slot.sub=nullptr;slot.state="UNKNOWN";
        if(sub)mem_deref(sub);
    }
}
int subscribe_parking() {
    if(!account_ua || !ua_isregistered(account_ua))return 0;
    const char *routev[1]={ua_outbound(account_ua)};
    int result=0;
    for(auto &slot:parking) {
        if(slot.number.empty() || slot.sub)continue;
        std::string uri=sip_uri(slot.number) ? slot.number : "sip:"+slot.number+"@"+authority+";transport="+sip_scheme;
        int err=sipevent_subscribe(&slot.sub,uag_sipevent_sock(),uri.c_str(),nullptr,
            account_aor(ua_account(account_ua)),"dialog",nullptr,600,ua_cuser(account_ua),
            routev,routev[0] ? 1 : 0,auth_handler,ua_account(account_ua),true,nullptr,
            parking_notify,parking_closed,&slot,"Accept: application/dialog-info+xml\r\n");
        if(err){slot.state="UNKNOWN";result=err;}
    }
    return result;
}
int login(re_printf *pf, void*) {
    if (account_ua) return EALREADY;
    // The address is policy data and arrives through the config. Only the user
    // name and its password come from the credential vault.
    char *server=nullptr,*extension=nullptr;
    uint32_t port=0;
    pl value=PL_INIT;
    if (!conf_get(conf_cur(),"ksip_sip_server",&value) && pl_isset(&value)) pl_strdup(&server,&value);
    value=PL_INIT;
    if (!conf_get(conf_cur(),"ksip_extension",&value) && pl_isset(&value)) pl_strdup(&extension,&value);
    char *transport=nullptr,*mediaenc=nullptr;
    value=PL_INIT;
    if (!conf_get(conf_cur(),"ksip_sip_transport",&value) && pl_isset(&value)) pl_strdup(&transport,&value);
    value=PL_INIT;
    if (!conf_get(conf_cur(),"ksip_mediaenc",&value) && pl_isset(&value)) pl_strdup(&mediaenc,&value);
    conf_get_u32(conf_cur(),"ksip_sip_port",&port);
    wchar_t target[200];
    DWORD n=GetEnvironmentVariableW(L"KSIP_CREDENTIAL_TARGET",target,RE_ARRAY_SIZE(target));
    int err=(!n || n>=RE_ARRAY_SIZE(target)) ? EINVAL : 0;
    PCREDENTIALW credential=nullptr;
    if (!err && !CredReadW(target,CRED_TYPE_GENERIC,0,&credential)) err=EACCES;
    std::string auth,password;
    if (!err) {
        if (credential->UserName) {
            int size=WideCharToMultiByte(CP_UTF8,0,credential->UserName,-1,nullptr,0,nullptr,nullptr);
            if (size>1) { auth.resize(size-1); WideCharToMultiByte(CP_UTF8,0,credential->UserName,-1,auth.data(),size,nullptr,nullptr); }
        }
        if (credential->CredentialBlobSize && credential->CredentialBlobSize<=1024)
            password.assign(reinterpret_cast<char*>(credential->CredentialBlob),credential->CredentialBlobSize);
        SecureZeroMemory(credential->CredentialBlob,credential->CredentialBlobSize);
        CredFree(credential);
    }
    // An empty extension means the account registers under its user name.
    std::string user=(extension && *extension) ? extension : auth;
    own_user=user;
    if (!err && (!token(server,".-") || !token(user.c_str(),"_.+-") || !token(auth.c_str(),"_.+-") ||
        password.empty() || password.size()>512 || !port || port>65535)) err=EINVAL;
    if (!err) {
        authority=std::string(server)+":"+std::to_string(port);
        // The transport belongs in the URI, the media encryption in the parameters.
        sip_scheme=(transport && !str_casecmp(transport,"TLS")) ? "tls" : (transport && !str_casecmp(transport,"TCP")) ? "tcp" : "udp";
        // The codecs the app chose, in its order; a name this build does not
        // know is skipped, and none at all means every codec in the usual order.
        std::string codecs;
        {
            pl list=PL_INIT;std::string names;
            if(!conf_get(conf_cur(),"ksip_audio_codecs",&list) && pl_isset(&list))names.assign(list.p,list.l);
            static const std::pair<const char*,const char*> known[]={{"opus","opus/48000/1"},{"G722","G722/16000/1"},{"PCMU","PCMU/8000/1"},{"PCMA","PCMA/8000/1"}};
            for(size_t start=0;start<=names.size();) {
                auto end=names.find(',',start);
                std::string name=names.substr(start,end==std::string::npos ? std::string::npos : end-start);
                for(auto &entry:known)if(name==entry.first && codecs.find(entry.second)==std::string::npos)codecs+=(codecs.empty() ? "" : ",")+std::string(entry.second);
                if(end==std::string::npos)break;
                start=end+1;
            }
            if(codecs.empty())codecs="opus/48000/1,G722/16000/1,PCMU/8000/1,PCMA/8000/1";
        }
        std::string aor="<sip:"+user+"@"+authority+";transport="+sip_scheme+">;regint=0;audio_codecs="+codecs+";answermode=manual;call_transfer=yes";
        media_encryption=(mediaenc && *mediaenc) ? mediaenc : "";
        if (mediaenc && *mediaenc) aor+=";mediaenc="+std::string(mediaenc);
        ua *created=nullptr;
        err=ua_alloc(&created,aor.c_str());
        if (!err) {
            err=account_set_auth_user(ua_account(created),auth.c_str());
            err|=account_set_auth_pass(ua_account(created),password.c_str());
            err|=account_set_regint(ua_account(created),register_interval);
            if (!err) { account_ua=created; registration="REGISTERING"; err=ua_register(created); }
            if (err) { account_ua=nullptr; ua_destroy(created); registration="REGISTER_FAIL"; }
        }
    }
    SecureZeroMemory(password.data(),password.size());
    mem_deref(server);
    mem_deref(extension);
    mem_deref(transport);
    mem_deref(mediaenc);
    if (!err) re_hprintf(pf,"Registration started\n");
    return err;
}
// The REFER of a transfer, once every hold sent for it has been answered (or
// the wait for them has run out): call 1 is referred to call 2, and a refusal
// is answered by trying the other way round once, as before.
// The REFER the other way round (call 2 referred to call 1), tried once when
// the first was refused. True when it went out.
bool refer_reversed(call *from, call *to) {
    transfer_reversed=true; std::swap(original,consultation);
    if (!call_replace_transfer(to,from)) return true;
    std::swap(original,consultation); return false;
}
void send_refer(void*) {
    if (!refer_armed) return;
    refer_armed=false; refer_waiting.clear();
    auto from=find(original); auto to=find(consultation);
    if (!from || !to || call_state(from)!=CALL_STATE_ESTABLISHED || call_state(to)!=CALL_STATE_ESTABLISHED) {
        clear_transfer(); set_outcome("TRANSFER_FAILED");
        if (from) uag_hold_resume(from);
        return;
    }
    if (call_replace_transfer(from,to) && !refer_reversed(from,to)) {
        clear_transfer(); set_outcome("TRANSFER_FAILED"); uag_hold_resume(from);
    }
}
void transfer_timeout(void*) {
    pending=false; refer_armed=false; set_outcome("TRANSFER_UNKNOWN");
    if (auto c=find(original)) uag_hold_resume(c);
}
// The server takes the other call over with the transfer and ends it itself.
// A BYE from here could reach it before it has done so and undo the transfer
// (a PBX that reports the transfer done the instant it accepts the REFER
// behaves that way), so the call is left to the server for a while.
void transfer_leftover(void*) {
    auto c=find(transfer_hangup);
    if (!c) { transfer_hangup.clear(); return; }
    info("ksip: the server left the transferred call up, ending it\n");
    ua_hangup(call_get_ua(c),c,0,nullptr);
}
void event(bevent_ev ev, bevent *e, void*) {
    if (bevent_get_ua(e)==account_ua) {
        if (ev==BEVENT_REGISTER_OK && !unregistered) {registration="REGISTER_OK";(void)subscribe_parking();(void)subscribe_mwi();}
        if (ev==BEVENT_REGISTER_FAIL) {registration="REGISTER_FAIL";registered_transport.clear();}
        if (ev==BEVENT_REGISTERING) registration="REGISTERING";
        if (ev==BEVENT_UNREGISTERING) {registration="UNREGISTERING";registered_transport.clear();}
    }
    auto c=bevent_get_call(e);
    if (!c || !call_id(c)) return;
    std::string id=call_id(c);
    if (ev==BEVENT_CALL_ESTABLISHED && talking_elsewhere(c)) {
        // The far end answered a call while the person was talking on another
        // one (they had gone back to it while this one rang). The person stays
        // where they are; this call waits on hold until they switch to it.
        info("ksip: call answered while another is active, holding it\n");
        call_hold(c,true);
    }
    if (ev==BEVENT_CALL_REMOTE_SDP && refer_armed && !str_cmp(bevent_get_text(e),"answer")) {
        // The answer to a hold sent for the transfer. Its ACK goes out right
        // after this event, so the REFER follows from the timer, not from here.
        refer_waiting.erase(std::remove(refer_waiting.begin(),refer_waiting.end(),id),refer_waiting.end());
        if (refer_waiting.empty()) tmr_start(&refer_timer,0,send_refer,nullptr);
    }
    if (ev==BEVENT_CALL_INCOMING && dnd) {
        // The app hears of it through CALL_INCOMING and the CALL_CLOSED that
        // follows, with these words; the state reply carries no list of its own.
        info("ksip: dnd, incoming call refused as busy\n");
        ua_hangup(bevent_get_ua(e),c,486,"Busy Here");
        return;
    }
    if (ev==BEVENT_CALL_TRANSFER_FAILED && id==original) {
        auto other=find(consultation);
        // Both calls are on hold by now (the REFER waited for that), so the
        // other way round can go out at once.
        if (!transfer_reversed && other && call_state(c)==CALL_STATE_ESTABLISHED
            && call_state(other)==CALL_STATE_ESTABLISHED && refer_reversed(c,other)) {
            tmr_start(&transfer_timer,60000,transfer_timeout,nullptr); return;
        }
        pending=false; tmr_cancel(&transfer_timer); set_outcome("TRANSFER_FAILED");
        uag_hold_resume(c);
    }
    if (ev==BEVENT_CALL_CLOSED && (id==original || id==consultation)) {
        std::string other=id==original ? consultation : original;
        bool completed=id==original && !str_cmp(bevent_get_text(e),"Call transfered");
        bool was_original=id==original;
        clear_transfer();
        set_outcome(completed ? "TRANSFER_DONE" : was_original ? "TRANSFER_ORIGINAL_CLOSED" : "TRANSFER_OTHER_CLOSED");
        if (auto remaining=find(other)) {
            if (completed) {
                transfer_hangup=other;
                info("ksip: transfer accepted, leaving the call with %s to the server\n",call_peeruri(remaining));
                tmr_start(&transfer_timer,5000,transfer_leftover,nullptr);
            }
            else uag_hold_resume(remaining);
        }
    }
    else if (ev==BEVENT_CALL_CLOSED && account_ua && !str_cmp(bevent_get_text(e),"Call transfered")) {
        // The transferred leg says so itself, whichever leg closed first. The
        // other one sometimes reports a reset before this arrives, and that
        // must not stand as the result.
        clear_transfer();
        set_outcome("TRANSFER_DONE");
    }
    else if (ev==BEVENT_CALL_CLOSED && account_ua) {
        // Calling a second person holds the first call. When the second one
        // ends, whether it was answered or still ringing, the first is brought
        // back here: baresip does not do that by itself, and the window says
        // the call was returned to.
        int others=0; call *remaining=nullptr;
        for (le *l=list_head(ua_calls(account_ua));l;l=l->next)
            if (static_cast<call*>(l->data)!=c) { ++others; remaining=static_cast<call*>(l->data); }
        if (id==transfer_hangup) { transfer_hangup.clear(); tmr_cancel(&transfer_timer); }
        else if (others==1 && call_state(remaining)==CALL_STATE_ESTABLISHED) {
            set_outcome("TRANSFER_OTHER_CLOSED");
            if (call_is_onhold(remaining)) uag_hold_resume(remaining);
        }
        else if (others==1) {
            // The remaining call is still ringing (either way); the window moves
            // to it, and says so with words that do not claim it was on hold.
            set_outcome("CALL_ENDED_SWITCHED");
        }
    }
    if(ev==BEVENT_CALL_CLOSED){connected_identity.erase(id);connected_name.erase(id);}
}
int state(re_printf *pf, void*) {
    odict *od=nullptr,*calls=nullptr,*xfer=nullptr;
    int err=odict_alloc(&od,16); err|=odict_alloc(&calls,16); err|=odict_alloc(&xfer,8);
    if (err) { mem_deref(od);mem_deref(calls);mem_deref(xfer);return err; }
    odict_entry_add(od,"registration",ODICT_STRING,registration.c_str());
    odict_entry_add(od,"dnd",ODICT_BOOL,dnd?1:0);
    odict_entry_add(od,"mwi_summary",ODICT_STRING,mwi_summary.c_str());
    odict_entry_add(od,"transport",ODICT_STRING,registered_transport.c_str());
    odict_entry_add(od,"media_encryption",ODICT_STRING,media_encryption.c_str());
    unsigned index=0;
    for (le *u=list_head(uag_list());u;u=u->next) {
        for (le *l=list_head(ua_calls(static_cast<ua*>(u->data)));l;l=l->next) {
            auto c=static_cast<call*>(l->data);
            if (call_state(c)==CALL_STATE_TERMINATED) continue;
            odict *entry=nullptr; if (odict_alloc(&entry,8)) continue;
            odict_entry_add(entry,"id",ODICT_STRING,call_id(c));
            auto identity=connected_identity.find(call_id(c));
            odict_entry_add(entry,"peer",ODICT_STRING,identity==connected_identity.end() ? call_peeruri(c) : identity->second.c_str());
            // The caller's name: the one PAI gave, else the one From gave.
            auto named=connected_name.find(call_id(c));
            pl from_name; pl_set_str(&from_name,call_peername(c) ? call_peername(c) : "");
            odict_entry_add(entry,"name",ODICT_STRING,(named==connected_name.end() ? display_name(from_name) : named->second).c_str());
            odict_entry_add(entry,"state",ODICT_STRING,call_statename(c));
            odict_entry_add(entry,"held",ODICT_BOOL,call_is_onhold(c));
            odict_entry_add(entry,"duration",ODICT_INT,static_cast<int64_t>(call_duration(c)));
            auto audio=call_audio(c);
            if (auto codec=audio ? audio_codec(audio,true) : nullptr) {
                char described[64];
                if (re_snprintf(described,sizeof(described),"%s %uHz",codec->name,codec->srate)>0)
                    odict_entry_add(entry,"codec",ODICT_STRING,described);
            }
            // True only once the encryption is actually established, which for
            // DTLS is a moment after the call is answered.
            odict_entry_add(entry,"secure",ODICT_BOOL,
                audio && stream_is_secure(audio_strm(audio)));
            odict_entry_add(entry,"transport",ODICT_STRING,sip_transp_name(call_transp(c)));
            odict_entry_add(calls,std::to_string(index++).c_str(),ODICT_OBJECT,entry);mem_deref(entry);
        }
    }
    odict *parks=nullptr;if(!odict_alloc(&parks,8)) {
        for(size_t i=0;i<parking.size();++i) {
            odict *entry=nullptr;if(odict_alloc(&entry,4))continue;
            odict_entry_add(entry,"number",ODICT_STRING,parking[i].number.c_str());
            odict_entry_add(entry,"state",ODICT_STRING,parking[i].state.c_str());
            odict_entry_add(parks,std::to_string(i).c_str(),ODICT_OBJECT,entry);mem_deref(entry);
        }
        odict_entry_add(od,"parking",ODICT_ARRAY,parks);mem_deref(parks);
    }
    odict_entry_add(xfer,"original",ODICT_STRING,original.c_str());
    odict_entry_add(xfer,"consultation",ODICT_STRING,consultation.c_str());
    odict_entry_add(xfer,"pending",ODICT_BOOL,pending);
    odict_entry_add(xfer,"outcome",ODICT_STRING,outcome.c_str());
    odict_entry_add(xfer,"outcome_seq",ODICT_INT,static_cast<int64_t>(outcome_seq));
    ksip_audio_stats stats{};
    if(index && !ksip_audio_get_current_stats(&stats) && stats.flags) {
        odict *aec=nullptr;
        if(!odict_alloc(&aec,24)) {
            if(stats.flags & KSIP_AUDIO_STATS_ECHO_RETURN_LOSS)odict_entry_add(aec,"echo_return_loss",ODICT_DOUBLE,stats.echo_return_loss);
            if(stats.flags & KSIP_AUDIO_STATS_ECHO_RETURN_LOSS_ENHANCEMENT)odict_entry_add(aec,"echo_return_loss_enhancement",ODICT_DOUBLE,stats.echo_return_loss_enhancement);
            if(stats.flags & KSIP_AUDIO_STATS_DELAY)odict_entry_add(aec,"delay_ms",ODICT_INT,static_cast<int64_t>(stats.delay_ms));
            if(stats.flags & KSIP_AUDIO_STATS_DIVERGENT_FILTER_FRACTION)odict_entry_add(aec,"divergent_filter_fraction",ODICT_DOUBLE,stats.divergent_filter_fraction);
            if(stats.flags & KSIP_AUDIO_STATS_DELAY_MEDIAN)odict_entry_add(aec,"delay_median_ms",ODICT_INT,static_cast<int64_t>(stats.delay_median_ms));
            if(stats.flags & KSIP_AUDIO_STATS_DELAY_STANDARD_DEVIATION)odict_entry_add(aec,"delay_standard_deviation_ms",ODICT_INT,static_cast<int64_t>(stats.delay_standard_deviation_ms));
            if(stats.flags & KSIP_AUDIO_STATS_RESIDUAL_ECHO_LIKELIHOOD)odict_entry_add(aec,"residual_echo_likelihood",ODICT_DOUBLE,stats.residual_echo_likelihood);
            if(stats.flags & KSIP_AUDIO_STATS_RESIDUAL_ECHO_LIKELIHOOD_RECENT_MAX)odict_entry_add(aec,"residual_echo_likelihood_recent_max",ODICT_DOUBLE,stats.residual_echo_likelihood_recent_max);
            if(stats.flags & KSIP_AUDIO_STATS_RENDER_LEVEL)odict_entry_add(aec,"render_rms_dbfs",ODICT_DOUBLE,stats.render_rms_dbfs);
            if(stats.flags & KSIP_AUDIO_STATS_CAPTURE_DEVICE_LEVEL)odict_entry_add(aec,"capture_device_rms_dbfs",ODICT_DOUBLE,stats.capture_device_rms_dbfs);
            if(stats.flags & KSIP_AUDIO_STATS_CAPTURE_MONO_LEVEL)odict_entry_add(aec,"capture_mono_rms_dbfs",ODICT_DOUBLE,stats.capture_mono_rms_dbfs);
            if(stats.flags & KSIP_AUDIO_STATS_CAPTURE_INPUT_LEVEL)odict_entry_add(aec,"capture_input_rms_dbfs",ODICT_DOUBLE,stats.capture_input_rms_dbfs);
            if(stats.flags & KSIP_AUDIO_STATS_CAPTURE_OUTPUT_LEVEL)odict_entry_add(aec,"capture_output_rms_dbfs",ODICT_DOUBLE,stats.capture_output_rms_dbfs);
            if(stats.flags & KSIP_AUDIO_STATS_AGC) {
                odict_entry_add(aec,"agc_speech_level_dbfs",ODICT_DOUBLE,stats.agc_speech_level_dbfs);
                odict_entry_add(aec,"agc_noise_level_dbfs",ODICT_DOUBLE,stats.agc_noise_level_dbfs);
                odict_entry_add(aec,"agc_headroom_db",ODICT_DOUBLE,stats.agc_headroom_db);
                odict_entry_add(aec,"agc_gain_db",ODICT_DOUBLE,stats.agc_gain_db);
            }
            odict_entry_add(aec,"stream_delay_ms",ODICT_INT,static_cast<int64_t>(stats.stream_delay_ms));
            odict_entry_add(aec,"stream_delay_from_device",ODICT_BOOL,stats.stream_delay_from_device != 0);
            odict_entry_add(aec,"render_frames",ODICT_INT,static_cast<int64_t>(stats.render_frames));
            odict_entry_add(aec,"capture_frames",ODICT_INT,static_cast<int64_t>(stats.capture_frames));
            odict_entry_add(aec,"render_errors",ODICT_INT,static_cast<int64_t>(stats.render_errors));
            odict_entry_add(aec,"capture_errors",ODICT_INT,static_cast<int64_t>(stats.capture_errors));
            odict_entry_add(aec,"capture_device_rate",ODICT_INT,static_cast<int64_t>(stats.capture_device_rate));
            odict_entry_add(aec,"capture_device_channels",ODICT_INT,static_cast<int64_t>(stats.capture_device_channels));
            odict_entry_add(od,"audio_processing_stats",ODICT_OBJECT,aec);mem_deref(aec);
        }
    }
    odict_entry_add(od,"calls",ODICT_ARRAY,calls);odict_entry_add(od,"transfer",ODICT_OBJECT,xfer);
    err=json_encode_odict(pf,od);mem_deref(xfer);mem_deref(calls);mem_deref(od);return err;
}
int action(re_printf *pf, void *arg) {
    auto a=static_cast<cmd_arg*>(arg); odict *od=nullptr;
    if (!a || !a->prm || json_decode_odict(&od,16,a->prm,strlen(a->prm),4)) return EINVAL;
    std::string op=odict_string(od,"op") ? odict_string(od,"op") : "";
    std::string id=odict_string(od,"id") ? odict_string(od,"id") : "";
    std::string value=odict_string(od,"value") ? odict_string(od,"value") : "";
    mem_deref(od);
    call *c=find(id);
    int err=0;
    // Hanging up and calling the transfer off are the two ways out of a pending transfer.
    if (pending && op!="hangup" && op!="cancel_transfer") return EBUSY;
    if (op=="select") {
        // A held call comes back and the active one goes on hold. The active
        // call itself is left alone: uag_hold_resume on a call that is not on
        // hold resumes whichever other call is, which swapped the two calls
        // when the person clicked the line they were already talking on.
        if (c && call_state(c)==CALL_STATE_ESTABLISHED) return call_is_onhold(c) ? uag_hold_resume(c) : 0;
        return hold_others(c);
    }
    if (op=="dial" || op=="consult") {
        if (!account_ua || !ua_isregistered(account_ua)) return EAGAIN;
        if (!original.empty() || !(sip_uri(value) ? address_ok(value) : token(value.c_str(),"*#+"))) return EINVAL;
        if (op=="consult" && (!c || call_state(c)!=CALL_STATE_ESTABLISHED || !call_supported(c,REPLACES))) return ENOTSUP;
        // A number is completed with the configured registrar; a URI is sent as
        // it was written. Either way the library has to be able to read it.
        std::string user; for (char ch:value) user+=ch=='#' ? "%23" : std::string(1,ch);
        std::string uri=sip_uri(value) ? value : "sip:"+user+"@"+authority+";transport="+sip_scheme;
        struct uri decoded; struct pl span; pl_set_str(&span,uri.c_str());
        if (uri_decode(&decoded,&span)) return EINVAL;
        for (le *l=list_head(ua_calls(account_ua));l;l=l->next) {
            auto other=static_cast<call*>(l->data);
            if (call_state(other)==CALL_STATE_ESTABLISHED && !call_is_onhold(other)) { c=other;err=call_hold(other,true);if(err)return err; }
        }
        call *next=nullptr;
        err=ua_connect(account_ua,&next,nullptr,uri.c_str(),VIDMODE_OFF);
        if (err) { if(c)uag_hold_resume(c); return err; }
        if (op=="consult") { original=id; consultation=call_id(next); outcome.clear(); }
        return re_hprintf(pf,"%s",call_id(next));
    }
    if (op=="transfer") {
        if (original.empty()) { original=id;consultation=value; }
        auto from=find(original);auto to=find(consultation);
        if (!from || !to || from==to || call_state(from)!=CALL_STATE_ESTABLISHED || call_state(to)!=CALL_STATE_ESTABLISHED) {clear_transfer();return EINVAL;}
        transfer_reversed=false;
        // As RFC 5589 has it: both calls on hold, then call 1 is referred to
        // call 2. The REFER waits until every hold sent here has been answered
        // and acknowledged. The production PBX ignored a REFER that arrived
        // while a hold re-INVITE on either call was still in progress, and
        // that is why its transfers did not go through; a hold that gets no
        // answer is waited for two seconds at most.
        refer_waiting.clear();
        for (call *each : {from,to})
            if (!call_is_onhold(each)) {
                err=call_hold(each,true);
                if (err) { clear_transfer(); return err; }
                refer_waiting.push_back(call_id(each));
            }
        pending=true;refer_armed=true;set_outcome("TRANSFER_PENDING");tmr_start(&transfer_timer,60000,transfer_timeout,nullptr);
        tmr_start(&refer_timer,refer_waiting.empty() ? 0 : 2000,send_refer,nullptr);
        return 0;
    }
    if (op=="cancel_transfer") {
        auto from=find(original);auto to=find(consultation);
        clear_transfer();set_outcome("TRANSFER_CANCELLED");
        if(to)ua_hangup(call_get_ua(to),to,0,nullptr);
        return from ? uag_hold_resume(from) : 0;
    }
    if(op=="blind_transfer") {
        // The call in progress is sent to the number as it is; the buttons
        // decide what the number means (a park slot, a colleague, a queue).
        if(!c || call_state(c)!=CALL_STATE_ESTABLISHED || call_is_onhold(c))return EINVAL;
        // A URI goes into Refer-To as it was set up, angle brackets included:
        // baresip copies what it can read as it is, and the PBX may only take
        // the form its phones send. A number is completed with the registrar.
        std::string bare=value;
        if(bare.size()>=2 && bare.front()=='<' && bare.back()=='>')bare=bare.substr(1,bare.size()-2);
        if(!(sip_uri(bare) ? address_ok(value) : token(value.c_str(),"*#+")))return EINVAL;
        std::string user; for (char ch:value) user+=ch=='#' ? "%23" : std::string(1,ch);
        std::string uri=sip_uri(bare) ? value : "sip:"+user+"@"+authority+";transport="+sip_scheme;
        return call_transfer(c,uri.c_str());
    }
    // Do not disturb is about the account as well: on or off, no call named.
    if (op=="dnd") { dnd=value=="on"; return 0; }
    // Unregistering is about the account, not about a call.
    if (op=="unregister") {
        if (!account_ua) return EINVAL;
        unregistered=true;
        ua_unregister(account_ua);
        registration="UNREGISTERED";
        registered_transport.clear();
        return 0;
    }
    if (!c) return ENOENT;
    if (op=="answer") { err=hold_others(c);return err ? err : ua_answer(call_get_ua(c),c,VIDMODE_OFF); }
    if (op=="hangup") {ua_hangup(call_get_ua(c),c,0,nullptr);return 0;}
    if (op=="hold") return call_state(c)==CALL_STATE_ESTABLISHED ? call_hold(c,true) : EINVAL;
    if (op=="resume") return call_state(c)==CALL_STATE_ESTABLISHED ? uag_hold_resume(c) : EINVAL;
    if (op=="dtmf" && value.size()==1 && strchr("0123456789*#",value[0])) {
        err=call_send_digit(c,value[0]);if(!err)err=call_send_digit(c,KEYCODE_REL);return err;
    }
    return EINVAL;
}
int configure_parking(re_printf *pf,void *arg) {
    auto a=static_cast<cmd_arg*>(arg);if(!a || !str_isset(a->prm))return EINVAL;
    // Up to six comma-separated numbers; an empty one is a slot nobody watches.
    std::array<std::string,30> values;std::string text=a->prm;size_t start=0;size_t count=0;
    for(;;) {
        if(count==values.size())return EINVAL;
        auto end=text.find(',',start);
        values[count]=text.substr(start,end==std::string::npos ? end : end-start);
        if(!values[count].empty() && !(sip_uri(values[count]) ? address_ok(values[count]) : (values[count].size()<=30 && token(values[count].c_str(),"*#+"))))return EINVAL;
        ++count;
        if(end==std::string::npos)break;
        start=end+1;
    }
    for(size_t i=0;i<values.size();++i)
        for(size_t j=i+1;j<values.size();++j)
            if(!values[i].empty() && values[i]==values[j])return EINVAL;
    clear_parking_subscriptions();for(size_t i=0;i<values.size()&&i<parking.size();++i)parking[i].number=values[i];
    int err=subscribe_parking();if(!err)re_hprintf(pf,"Parking subscriptions configured\n");return err;
}
// The microphone and speaker for the calls from now on, as endpoint ids, so
// that a change of device does not need the engine restarted. baresip reads
// the devices out of its configuration when a call's audio starts, so the
// configuration is what changes here; a call that is up keeps the devices it
// opened, and the app does not switch while one is up. baresip's own auplay
// command would also move the alert sounds onto the call's player, which
// only takes the call's 48 kHz, so the alert stays with its own module and
// just follows the speaker.
int audio_devices(re_printf *pf,void *arg) {
    auto a=static_cast<cmd_arg*>(arg);if(!a || !str_isset(a->prm))return EINVAL;
    std::string text=a->prm;auto comma=text.find(',');if(comma==std::string::npos)return EINVAL;
    std::string microphone=text.substr(0,comma),speaker=text.substr(comma+1);
    config *cfg=conf_config();if(!cfg)return ENOENT;
    for(const auto &device:{microphone,speaker}) {
        if(device.empty() || device.size()>=sizeof(cfg->audio.play_dev))return EINVAL;
        for(unsigned char ch:device)if(ch<0x20 || ch==0x7f || ch==',')return EINVAL;
    }
    str_ncpy(cfg->audio.src_dev,microphone.c_str(),sizeof(cfg->audio.src_dev));
    str_ncpy(cfg->audio.play_dev,speaker.c_str(),sizeof(cfg->audio.play_dev));
    str_ncpy(cfg->audio.alert_dev,speaker.c_str(),sizeof(cfg->audio.alert_dev));
    info("ksip: audio devices from the next call on, microphone %s, speaker %s\n",microphone.c_str(),speaker.c_str());
    return re_hprintf(pf,"Audio devices set\n");
}
int shutdown(re_printf *pf,void*) {
    // Each subscription ended here is a request the server still has to
    // answer before baresip can quit; the count says what a slow quit waits on.
    size_t watched=0;for(auto &slot:parking)if(slot.sub)++watched;
    info("ksip: shutdown, ending %zu parking subscriptions%s\n",watched,mwi_sub ? " and the mwi subscription" : "");
    clear_parking_subscriptions();
    return re_hprintf(pf,"KSIP subscriptions closed\n");
}
const cmd commands[]={
    {"ksip_login",0,0,"Load Windows SIP credential and register",login},
    {"ksip_state",0,0,"Get account and per-call state",state},
    {"ksip_action",0,CMD_PRM,"Operate a specific call",action},
    {"ksip_parking",0,CMD_PRM,"Watch up to six numbers through dialog-state subscriptions",configure_parking},
    {"ksip_shutdown",0,0,"Release KSIP subscriptions before quit",shutdown},
    {"ksip_audio_devices",0,CMD_PRM,"Use these microphone and speaker endpoint ids from the next call on",audio_devices},
};
int init(){
    uint32_t interval=register_interval;
    if(!conf_get_u32(conf_cur(),"ksip_register_interval",&interval) && interval>=30 && interval<=3600)register_interval=interval;
    // One switch covers both kinds of detail: the SIP messages and the debug
    // level. libre is compiled with DEBUG_LEVEL 5, so its debug lines do not
    // exist in this build and only baresip has a level left to raise.
    bool detail=false;
    conf_get_bool(conf_cur(),"ksip_detail_log",&detail);
    sip_message_log=detail;
    if(detail)log_enable_debug(true);
    tmr_init(&transfer_timer);tmr_init(&parking_timer);sip_set_trace_handler(uag_sip(),sip_trace);int err=bevent_register(event,nullptr);if(!err)err=cmd_register(baresip_commands(),commands,RE_ARRAY_SIZE(commands));return err;}
int close(){sip_set_trace_handler(uag_sip(),nullptr);connected_identity.clear();connected_name.clear();clear_transfer();clear_parking_subscriptions();bevent_unregister(event);cmd_unregister(baresip_commands(),commands);account_ua=nullptr;return 0;}
}
extern "C" const struct mod_export DECL_EXPORTS(ksip)={"ksip","application",init,close};
