# The WebRTC CoreAudio Start() of the tree as patched, run on the audio thread that holds its own handle as an internal restart does, with IAudioClient::Start failing: it must return, not wait for its own end (the start-failure change in scripts/build/patch-webrtc.py); and after a start that failed, the bridge's next start must release the initialization the failure left behind and open the device (ksip_audio_bridge.cc); needs MSVC and the WebRTC source native.ps1 fetches and patches.
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/../dev-env.ps1"
Set-Location (Split-Path (Split-Path $PSScriptRoot))
$base = 'temp/w/src/modules/audio_device/win/core_audio_base_win.cc'
$threads = 'temp/w/src/rtc_base/platform_thread.cc'
if (!(Test-Path $base) -or !(Test-Path $threads)) { Write-Host 'SKIP: no WebRTC source tree (scripts/build/native.ps1 fetches and patches it)'; exit 0 }
New-Item -ItemType Directory -Force temp/build, temp/reports | Out-Null

# One function of a source file, from its signature to the closing brace in column one.
function Extract([string]$text, [string]$signature) {
    $start = $text.IndexOf($signature)
    if ($start -lt 0) { throw "$signature not found" }
    $end = $text.IndexOf("`n}`n", $start)
    return $text.Substring($start, $end - $start + 3)
}
$source = (Get-Content -Raw $base) -replace "`r`n", "`n"
$start = Extract $source 'bool CoreAudioBase::Start()'
$stopThread = Extract $source 'void CoreAudioBase::StopThread()'
$finalize = (Extract ((Get-Content -Raw $threads) -replace "`r`n", "`n") 'void PlatformThread::Finalize()').Replace('void PlatformThread::Finalize()', 'void webrtc::PlatformThread::Finalize()')

# What those functions need of WebRTC, as little of it as makes them build: a
# client whose Start fails, and the real Windows thread handle and wait.
$prefix = @'
#include <windows.h>
#include <cstdio>
#include <optional>
#include <string_view>
#include <utility>
struct Log { template <class T> Log &operator<<(const T &) { return *this; } };
#define RTC_DLOG(x) Log{}
#define RTC_LOG(x) Log{}
#define RTC_DCHECK(x) ((void)0)
#define LS_INFO 0
#define LS_ERROR 0
#define WEBRTC_WIN
namespace absl { using string_view = std::string_view; }
struct _com_error {
    HRESULT code;
    _com_error(HRESULT h) : code(h) {}
    HRESULT Error() const { return code; }
};
namespace core_audio_utility { const char *ErrorToString(_com_error) { return "injected start failure"; } }
namespace webrtc {
enum class ThreadPriority { kRealtime };
struct ThreadAttributes { ThreadAttributes SetPriority(ThreadPriority) { return *this; } };
struct PlatformThread {
    std::optional<HANDLE> handle_;
    bool joinable_ = true;
    bool empty() const { return !handle_; }
    std::optional<HANDLE> GetHandle() const { return handle_; }
    template <class F> static PlatformThread SpawnJoinable(F, absl::string_view, ThreadAttributes) { return {}; }
    void Finalize();
};
}
'@
$middle = @'
struct Event {
    HANDLE h = CreateEvent(nullptr, TRUE, FALSE, nullptr);
    HANDLE Get() { return h; }
};
struct Client { HRESULT Start() { return E_FAIL; } };
struct Clock { long long TimeInMilliseconds() { return 0; } };
struct Env { Clock c; Clock &clock() { return c; } };
struct CoreAudioBase {
    bool restarting = true;
    webrtc::PlatformThread audio_thread_;
    Event stop_event_, restart_event_;
    Client client;
    Client *audio_client_ = &client;
    Env env_;
    long long start_time_ = 0;
    int num_data_callbacks_ = 0;
    bool IsRestarting() const { return restarting; }
    bool IsInput() const { return true; }
    int direction() const { return 0; }
    const char *DirectionToString(int) const { return "input"; }
    void ThreadRun() {}
    bool Start();
    void StopThread();
};
'@
$main = @'
int main(int argc, char **argv) {
    CoreAudioBase core;
    // The internal restart runs on this, the audio thread, which holds its
    // own joinable handle; "no-handle" is the case with none, for contrast.
    const bool self = argc < 2 || std::string_view(argv[1]) != "no-handle";
    if (self) core.audio_thread_.handle_ = OpenThread(SYNCHRONIZE, FALSE, GetCurrentThreadId());
    std::printf("restarting=%d self_handle=%d: Start() with IAudioClient::Start = E_FAIL\n", core.restarting, self);
    std::fflush(stdout);
    const bool result = core.Start();
    std::printf("returned=%d\n", result);
    return 0;
}
'@
$code = $prefix + "`n" + $finalize + "`n" + $middle + "`n" + $start + "`n" + $stopThread + "`n" + $main
[System.IO.File]::WriteAllText("$PWD/temp/build/test-webrtc-restart.cpp", $code, (New-Object System.Text.UTF8Encoding $false))
cl /nologo /std:c++20 /EHsc /O2 /MT /DNOMINMAX /utf-8 temp/build/test-webrtc-restart.cpp /Fetemp/build/test-webrtc-restart.exe /Fotemp/build/ /link /SUBSYSTEM:CONSOLE
if ($LASTEXITCODE -ne 0) { throw 'test-webrtc-restart did not build' }

# Bounded: before the change, the case with its own handle never returned.
foreach ($case in @('self-handle', 'no-handle')) {
    $out = "temp/reports/webrtc-restart-$case.txt"
    $p = Start-Process -FilePath "$PWD/temp/build/test-webrtc-restart.exe" -ArgumentList $case -NoNewWindow -PassThru -RedirectStandardOutput $out
    # The handle read now, or the exit code is not filled in later.
    $null = $p.Handle
    if (!$p.WaitForExit(5000)) {
        $p.Kill()
        throw "Start() did not return on the audio thread ($case): it waits for its own end"
    }
    $p.WaitForExit()
    if ($p.ExitCode -ne 0) { throw "test-webrtc-restart exited with $($p.ExitCode) ($case)" }
    if (!(Select-String -Path $out -Pattern 'returned=0' -Quiet)) { throw "Start() did not report its failure ($case): $(Get-Content $out)" }
}
Write-Host 'PASS: a Start() that fails during an internal restart returns to the audio thread, with and without its own handle held'

# After a start that failed, the stream is initialized but not recording,
# and SetDevice refuses an initialized stream: the bridge's start and stop
# of the tree as it is, with the pinned CoreAudio transitions, must release
# that before the next device is set. The bridge's functions are extracted
# as they are; what they need of WebRTC and of the bridge's state is faked.
$input = 'temp/w/src/modules/audio_device/win/core_audio_input_win.cc'
$bridge = 'src-native/webrtc/ksip_audio_bridge.cc'
$inputSource = (Get-Content -Raw $input) -replace "`r`n", "`n"
$bridgeSource = (Get-Content -Raw $bridge) -replace "`r`n", "`n"
$setDevice = Extract $source 'int CoreAudioBase::SetDevice(int index)'
$startRecording = Extract $inputSource 'int CoreAudioInput::StartRecording()'
$stopRecording = Extract $inputSource 'int CoreAudioInput::StopRecording()'
$bridgeStart = Extract $bridgeSource 'extern "C" int ksip_audio_start_recording('
$bridgeStop = Extract $bridgeSource 'extern "C" void ksip_audio_stop_recording('
$recoverPrefix = @'
#include <cstdio>
#include <string>
#include <mutex>
struct Log { template <class T> Log &operator<<(const T &) { return *this; } };
#define RTC_DLOG(x) Log{}
#define RTC_LOG(x) Log{}
#define RTC_DCHECK(x) ((void)0)
#define RTC_DCHECK_RUN_ON(x) ((void)0)
#define LS_INFO 0
#define LS_WARNING 0
#define LS_ERROR 0
struct Buffer { void ResetRecord() {} void StartRecording() {} void StopRecording() {} void reset() {} };
struct CoreAudioBase {
    bool initialized_ = true;
    int device_index_ = 0;
    std::string device_id_ = "microphone-a";
    int direction() { return 0; }
    const char *DirectionToString(int) { return "input"; }
    int IndexToString(int i) { return i; }
    std::string GetDeviceID(int) { return "microphone-b"; }
    int SetDevice(int index);
};
struct CoreAudioInput : CoreAudioBase {
    bool is_active_ = false, failed = true, restarting = true;
    Buffer buffer, qpc_to_100ns_;
    Buffer *fine_audio_buffer_ = &buffer, *audio_device_buffer_ = &buffer;
    bool IsRestarting() { return restarting; }
    bool Recording() { return is_active_; }
    bool RecordingIsInitialized() const { return initialized_; }
    bool Start() { return !failed; }
    bool Stop() { return true; }
    void ReleaseCOMObjects() {}
    int StartRecording();
    int StopRecording();
};
struct Adm {
    CoreAudioInput core;
    bool Recording() { return core.Recording(); }
    bool RecordingIsInitialized() { return core.RecordingIsInitialized(); }
    bool Playing() { return true; }
    int InitRecording() { core.initialized_ = true; return 0; }
    int StartRecording() { return core.StartRecording(); }
    int StopRecording() { return core.StopRecording(); }
};
using ksip_audio_capture_cb = void (*)(void *, const short *, size_t, long long);
struct Gate { void Set(ksip_audio_capture_cb, void *) {} void ClearAndDrain() {} };
struct ksip_audio {
    Adm instance;
    Adm *adm = &instance;
    std::mutex device_mutex;
    std::string recording_request = "default";
    Gate capture_gate;
    bool Serves(const std::string &, bool) { return true; }
    int ResetDiagnostics() { return 0; }
    int SetDevice(const char *, bool) { return adm->core.SetDevice(1); }
};
void ClearOpened(bool) {}
void cb(void *, const short *, size_t, long long) {}
extern "C" void ksip_audio_stop_recording(ksip_audio *);
'@
$recoverMain = @'
int main() {
    ksip_audio a;
    // The start that fails (the driver refuses), as an internal restart's does: initialized, not recording.
    const int first = a.adm->core.StartRecording();
    a.adm->core.failed = false;
    a.adm->core.restarting = false;
    // The driver healthy again: the bridge's next start must open the device.
    const int retry = ksip_audio_start_recording(&a, "microphone-b", cb, nullptr);
    const bool recording = a.adm->Recording();
    ksip_audio_stop_recording(&a);
    const int again = ksip_audio_start_recording(&a, "microphone-b", cb, nullptr);
    std::printf("start failed=%d initialized=%d; the bridge's next start=%d recording=%d; after a stop, again=%d\n",
                first, a.adm->core.initialized_, retry, recording, again);
    return (first == -1 && retry == 0 && recording && again == 0) ? 0 : 1;
}
'@
$code = $recoverPrefix + "`n" + $setDevice + "`n" + $startRecording + "`n" + $stopRecording + "`n" + $bridgeStart + "`n" + $bridgeStop + "`n" + $recoverMain
[System.IO.File]::WriteAllText("$PWD/temp/build/test-webrtc-recover.cpp", $code, (New-Object System.Text.UTF8Encoding $false))
cl /nologo /std:c++20 /EHsc /O2 /MT /DNOMINMAX /utf-8 temp/build/test-webrtc-recover.cpp /Fetemp/build/test-webrtc-recover.exe /Fotemp/build/ /link /SUBSYSTEM:CONSOLE
if ($LASTEXITCODE -ne 0) { throw 'test-webrtc-recover did not build' }
& temp/build/test-webrtc-recover.exe | Tee-Object -FilePath temp/reports/webrtc-recover.txt
if ($LASTEXITCODE -ne 0) { throw 'after a start that failed, the bridge did not open the device again' }
Write-Host 'PASS: after a start that failed, the bridge releases the initialization left behind and opens the device'
