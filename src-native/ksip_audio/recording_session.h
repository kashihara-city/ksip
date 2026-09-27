// Which call is being recorded, and into which file: the recording follows
// the call the app names, waits for a call that has no audio yet, and stays
// with a call whose decoder baresip remakes for a codec change. The samples
// of both sides go to the Recorder; the filters bring them here.
//
// Threads: far_frame and near_frame come from baresip's audio threads (one
// each) and take the gate for the length of a push; everything else comes
// on baresip's main thread. Lifetime: module-static state for the one
// module instance; close() finishes whatever is being recorded and is the
// last call, after the filter is unregistered so no frame follows it. The
// outcome of a recording (bytes, dropped samples, a write error) is the
// stop command's answer; the log line beside it is for reading, not parsing.
#pragma once
#include <cmath>
#include <algorithm>
#include <re.h>
#include <rem.h>
#include <baresip.h>

namespace recording_session {
// A call's decode filter was made or taken away: the session keeps track of
// the calls that carry audio.
void decoder_created(const audio *stream, uint32_t rate);
void decoder_destroyed(const audio *stream);
// A frame of the far end, and a frame of this side, for the named call.
void far_frame(const audio *stream, const auframe *frame);
void near_frame(const audio *stream, const auframe *frame);
// The commands: start a WAV for a call (or the only call), finish it, and
// switch the call it follows ("-" pauses the input).
int start(re_printf *pf, const char *prm);
int stop(re_printf *pf);
int select(re_printf *pf, const char *prm);
// The module closes: whatever is being recorded is finished.
void close();
} // namespace recording_session
