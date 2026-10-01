// The microphone's own mute (Windows', the one the tray flyout, the app's
// mute button and a headset's button all set) carried into the calls: while
// the endpoint the bridge records from is muted, every call's audio is muted
// in baresip too. baresip's mute sends silence, zeros encoded as usual, so
// RTP goes on (a PBX that waits for RTP, and a firewall that keeps the way
// in only while packets go out, see no difference); what a muted device
// still delivers (one tried left noise at -66 dBFS) goes no further.
//
// Threads: baresip's main thread only (its timer). Lifetime: start() once
// the bridge is up, stop() before it goes (device_module.cpp).
#pragma once

struct ksip_audio;

namespace microphone_mute {
void start(ksip_audio *bridge);
void stop();
} // namespace microphone_mute
