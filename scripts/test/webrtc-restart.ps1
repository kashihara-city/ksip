# The WebRTC CoreAudio Start() of the tree as patched, run on the audio thread that holds its own handle as an internal restart does, with IAudioClient::Start failing: it must return, not wait for its own end (the start-failure change in scripts/build/patch-webrtc.py); needs MSVC and the WebRTC source native.ps1 fetches and patches.
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
