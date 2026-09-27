// A callback slot that can be cleared safely while the callback may be
// running on another thread. The audio thread holds the in-flight lock for
// the whole of a call; whoever clears the slot takes that lock once
// afterwards, so it returns only when no call with the old argument is still
// running, and may then free what the argument points to. Cleared from the
// callback's own thread it only clears, since the call in flight is the
// caller itself. Nothing here knows WebRTC or baresip.
#pragma once
#include <atomic>
#include <mutex>
#include <thread>

namespace ksip_audio_internal {
template <typename Callback>
class CallbackGate {
 public:
  // Installs a callback with its argument; a running call goes on with the
  // pair it read.
  void Set(Callback callback, void *arg) {
    std::lock_guard<std::mutex> lock(slot_mutex);
    callback_ = callback;
    arg_ = arg;
  }
  bool Installed() {
    std::lock_guard<std::mutex> lock(slot_mutex);
    return callback_ != nullptr;
  }
  // Runs `call(callback, arg)` under the in-flight lock, with the pair as it
  // is at that moment (null when nothing is installed). The in-flight lock
  // comes first, then the slot is read: a clear that came before is not seen
  // with a stale argument, and one that comes after waits for this to end.
  template <typename Call>
  void Invoke(Call call) {
    std::lock_guard<std::mutex> in_flight(call_mutex);
    call_thread.store(std::this_thread::get_id());
    Callback callback;
    void *arg;
    {
      std::lock_guard<std::mutex> lock(slot_mutex);
      callback = callback_;
      arg = arg_;
    }
    call(callback, arg);
    call_thread.store(std::thread::id{});
  }
  // Clears the slot and returns once no call of it is still running.
  void ClearAndDrain() {
    {
      std::lock_guard<std::mutex> lock(slot_mutex);
      callback_ = nullptr;
      arg_ = nullptr;
    }
    if (call_thread.load() == std::this_thread::get_id()) return;
    std::lock_guard<std::mutex> in_flight(call_mutex);
  }

 private:
  std::mutex slot_mutex;
  Callback callback_ = nullptr;
  void *arg_ = nullptr;
  std::mutex call_mutex;
  std::atomic<std::thread::id> call_thread{};
};
}  // namespace ksip_audio_internal
