// libre's timers as the session core's Clock (session_core.h): one timer
// for each of the core's, firing `fire(which)` on baresip's main thread. A
// core and its clock are made once at module load; the clock's timers are
// cancelled at module close (stop), before libre's loop is gone.
#pragma once
#include <functional>
#include <re.h>
#include "session_core.h"

namespace playback_session {
class LibreClock final : public Clock {
public:
    std::function<void(Timer)> fire;
    LibreClock() {
        const Timer timers[] = {Timer::Linger, Timer::HandBack, Timer::Watch};
        for (size_t i = 0; i < 3; ++i) {
            tmr_init(&slots[i].timer);
            slots[i].clock = this;
            slots[i].which = timers[i];
        }
    }
    void start(Timer which, uint64_t ms) override {
        Slot &s = slot(which);
        tmr_start(&s.timer, ms, fired, &s);
    }
    void cancel(Timer which) override { tmr_cancel(&slot(which).timer); }
    void cancel_all() {
        for (Slot &s : slots) tmr_cancel(&s.timer);
    }

private:
    struct Slot {
        tmr timer;
        LibreClock *clock;
        Timer which;
    };
    Slot slots[3]{};
    Slot &slot(Timer which) { return slots[static_cast<int>(which)]; }
    static void fired(void *arg) {
        auto *s = static_cast<Slot *>(arg);
        if (s->clock->fire) s->clock->fire(s->which);
    }
};
} // namespace playback_session
