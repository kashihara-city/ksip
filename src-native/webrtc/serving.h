// Whether a stream that runs for a request serves it as one opened for it
// now would, decided from what Windows lists. Nothing here knows WebRTC, so
// that the decision is tested on its own (test-ksip-audio.cpp);
// device_selection.cc reads the listing and acts on the answer. The app
// hands the engine its saved devices again whenever Windows' devices
// change, and this is what makes that an order to move, or nothing.
#pragma once
#include <algorithm>
#include <string>
#include <vector>

namespace ksip_audio_bridge {
// Why a running stream is opened again, or that it is kept as it is.
enum class Serving {
    // The stream serves the request as it is.
    kKept,
    // The endpoint in use is not listed, or not known: WebRTC counts a stream
    // as running still when it could not move it off a device that went (no
    // other device to move to), and nothing comes through it.
    kEndpointGone,
    // "default" asked for, and the default stands for another endpoint now
    // (the person changed it, the device set as it is back): WebRTC does not
    // follow that, it restarts a stream only when its device goes.
    kDefaultMoved,
    // The device asked for is listed, and the stream is on another: the
    // default in its place while it was not there, or wherever WebRTC moved
    // the stream when the device went.
    kDeviceBack,
};
// `request`: the device asked for, "default" or an endpoint id. `opened`:
// the endpoint WebRTC says it opened, empty while that is not known.
// `listed`: the ids of the side's endpoints now. `default_now`: the id the
// default stands for, empty while there is none (nothing to follow then).
inline Serving serving(const std::string &request, const std::string &opened, const std::vector<std::string> &listed,
                       const std::string &default_now) {
    const auto is_listed = [&listed](const std::string &id) { return std::find(listed.begin(), listed.end(), id) != listed.end(); };
    if (opened.empty() || !is_listed(opened)) return Serving::kEndpointGone;
    if (request == "default") return default_now.empty() || opened == default_now ? Serving::kKept : Serving::kDefaultMoved;
    if (opened == request || !is_listed(request)) return Serving::kKept;
    return Serving::kDeviceBack;
}
} // namespace ksip_audio_bridge
