// Which endpoint the ADM opens for a request: the Windows default
// communications device, or the endpoint whose id was asked for, matched
// among everything WebRTC lists (the default in its place while it is not
// there); and the name and id of what was opened, for the app to show and
// for the microphone mute to read.
#include "bridge_state.h"
#include "serving.h"

#include <cstdio>
#include <cstring>
#include <string>
#include <vector>

#include "modules/audio_device/win/core_audio_utility_win.h"
#include "rtc_base/logging.h"

using ksip_audio_bridge::kRoleEntries;

namespace {
// Every endpoint WebRTC lists for the side, in the order its indexes go (the
// two role entries first, with the id of the endpoint each stands for), read
// in one pass. WebRTC's own PlayoutDeviceName(i) enumerates everything, with
// each name read off the property store, afresh for every index: asked for
// every endpoint, that stalled baresip's thread for hundreds of milliseconds
// on a machine with several devices.
bool ListAll(bool playout, webrtc::AudioDeviceNames &names) {
  names.clear();
  return playout ? webrtc::webrtc_win::core_audio_utility::GetOutputDeviceNames(&names)
                 : webrtc::webrtc_win::core_audio_utility::GetInputDeviceNames(&names);
}
}  // namespace

int ksip_audio::SelectDefault(bool playout) {
  const int result = playout
      ? adm->SetPlayoutDevice(webrtc::AudioDeviceModule::kDefaultCommunicationDevice)
      : adm->SetRecordingDevice(webrtc::AudioDeviceModule::kDefaultCommunicationDevice);
  if (result) return result;
  // The name and id are those of the list's communications role entry
  // (kCommunicationsEntry: core_audio_utility_win.cc). Asked for at index -1
  // (the device type's value), the name was never there.
  webrtc::AudioDeviceNames names;
  if (ListAll(playout, names) && names.size() > ksip_audio_bridge::kCommunicationsEntry) {
    const webrtc::AudioDeviceName &entry = names[ksip_audio_bridge::kCommunicationsEntry];
    StoreDevice(playout, entry.device_name.c_str(), entry.unique_id.c_str());
  }
  return 0;
}

int ksip_audio::SetDevice(const char *id, bool playout) {
  if (!id || !id[0] || std::strcmp(id, "default") == 0) return SelectDefault(playout);
  // PlayoutDevices() and RecordingDevices() count the endpoints, but the
  // list that PlayoutDeviceName() and RecordingDeviceName() index has the
  // two role entries in front of them. A loop over the count alone never
  // reached the last two endpoints, so a chosen device that was neither a
  // Windows default nor early in the list counted as not there, and the
  // call failed with no device. The endpoints themselves are matched first,
  // so that a chosen device stays chosen when Windows moves its defaults;
  // a role entry only serves as a fallback.
  webrtc::AudioDeviceNames names;
  ListAll(playout, names);
  const int count = static_cast<int>(names.size()) - kRoleEntries;
  int role_index = -1;
  std::string listing;
  for (int i = 0; i < static_cast<int>(names.size()); ++i) {
    const webrtc::AudioDeviceName &entry = names[static_cast<size_t>(i)];
    listing += " [" + std::to_string(i) + "] " + entry.device_name + " " + entry.unique_id;
    if (entry.unique_id != id) continue;
    if (i < kRoleEntries) {
      if (role_index < 0) role_index = i;
      continue;
    }
    return Select(playout, i, entry.device_name.c_str(), entry.unique_id.c_str());
  }
  if (role_index >= 0) {
    const webrtc::AudioDeviceName &entry = names[static_cast<size_t>(role_index)];
    return Select(playout, role_index, entry.device_name.c_str(), entry.unique_id.c_str());
  }
  // A chosen device that is not there (unplugged, its hub without power,
  // never on this machine): the call goes through the Windows default
  // communications device, as the alert sounds do (alert_player.h) and the
  // window's volume shows, rather than failing with
  // no device, which ended an outgoing call at its answer and left an
  // incoming one silent both ways. The request stays the chosen device, so
  // the next stream once it is back opens it again. Written at warning
  // level, which the app's log always carries, so that the device in use is
  // explained.
  RTC_LOG(LS_WARNING) << "ksip_audio: " << (playout ? "playout" : "recording")
                      << " device " << id << " is not among the " << count
                      << " endpoints WebRTC lists:" << listing
                      << "; using the default communications device in its place";
  return SelectDefault(playout) ? -5 : 0;
}

bool ksip_audio::Listed(const char *id, bool playout) {
  webrtc::AudioDeviceNames names;
  if (!ListAll(playout, names)) return false;
  for (const webrtc::AudioDeviceName &entry : names)
    if (entry.unique_id == id) return true;
  return false;
}

// The endpoint the default stands for now: the id of the list's
// communications role entry; empty while there is none.
std::string ksip_audio::DefaultEndpoint(bool playout) {
  return playout ? webrtc::webrtc_win::core_audio_utility::GetCommunicationsOutputDeviceID()
                 : webrtc::webrtc_win::core_audio_utility::GetCommunicationsInputDeviceID();
}

// Whether a stream running for `request` serves it as one opened for it
// now would (serving.h decides; the cases are described there). What
// Windows lists is read once: every endpoint of the side, and the one the
// default stands for (the list's communications role entry).
bool ksip_audio::Serves(const std::string &request, bool playout) {
  const std::string opened = ksip_audio_bridge::OpenedEndpoint(playout);
  webrtc::AudioDeviceNames names;
  std::vector<std::string> listed;
  std::string default_now;
  if (ListAll(playout, names)) {
    for (const webrtc::AudioDeviceName &entry : names) listed.push_back(entry.unique_id);
    if (names.size() > ksip_audio_bridge::kCommunicationsEntry) default_now = names[ksip_audio_bridge::kCommunicationsEntry].unique_id;
  }
  const char *side = playout ? "playout" : "recording";
  switch (ksip_audio_bridge::serving(request, opened, listed, default_now)) {
    case ksip_audio_bridge::Serving::kKept:
      return true;
    case ksip_audio_bridge::Serving::kEndpointGone:
      RTC_LOG(LS_WARNING) << "ksip_audio: the " << side << " endpoint in use, "
                          << (opened.empty() ? std::string("unknown") : opened) << ", is gone; the stream is opened again";
      return false;
    case ksip_audio_bridge::Serving::kDefaultMoved:
      RTC_LOG(LS_WARNING) << "ksip_audio: the default " << side << " device is " << default_now << " now, not " << opened
                          << "; the stream is opened on it";
      return false;
    case ksip_audio_bridge::Serving::kDeviceBack:
      RTC_LOG(LS_WARNING) << "ksip_audio: " << side << " device " << request << " is back, the stream is opened on it again";
      return false;
  }
  return false;
}

int ksip_audio::Select(bool playout, int index, const char *name, const char *guid) {
  const int result = playout
      ? adm->SetPlayoutDevice(static_cast<uint16_t>(index))
      : adm->SetRecordingDevice(static_cast<uint16_t>(index));
  if (result) {
    RTC_LOG(LS_WARNING) << "ksip_audio: selecting "
                        << (playout ? "playout" : "recording") << " device ["
                        << index << "] " << name << " failed (" << result << ")";
    return result;
  }
  StoreDevice(playout, name, guid);
  return 0;
}

void ksip_audio::StoreDevice(bool playout, const char *name, const char *id) {
  std::lock_guard<std::mutex> lock(device_mutex);
  auto &stored_name = playout ? playout_name : recording_name;
  auto &stored_id = playout ? playout_id : recording_id;
  stored_name = name ? name : "";
  stored_id = id ? id : "";
}

extern "C" int ksip_audio_endpoint_listed(ksip_audio *audio, const char *id, int playout) {
  if (!audio || !id || !id[0]) return 0;
  if (std::strcmp(id, "default") == 0) {
    // There while the side has any endpoint at all for it to stand for.
    const int count = playout ? audio->adm->PlayoutDevices() : audio->adm->RecordingDevices();
    return count > 0 ? 1 : 0;
  }
  return audio->Listed(id, playout != 0) ? 1 : 0;
}

extern "C" int ksip_audio_default_endpoint(ksip_audio *audio, int playout, char *out, size_t size) {
  if (!audio || !out || !size) return 0;
  const std::string id = audio->DefaultEndpoint(playout != 0);
  std::snprintf(out, size, "%s", id.c_str());
  return id.empty() ? 0 : 1;
}

extern "C" int ksip_audio_get_device_info(ksip_audio *audio,
                                            ksip_audio_device_info *info) {
  if (!audio || !info) return -1;
  // What is open on each side: the endpoint WebRTC last opened, which is not
  // the one selected when WebRTC moved to the default by itself (the device
  // in use went away); its name is not known here then. Before any stream,
  // what was selected.
  const std::string opened_recording = ksip_audio_bridge::OpenedEndpoint(false);
  const std::string opened_playout = ksip_audio_bridge::OpenedEndpoint(true);
  std::lock_guard<std::mutex> lock(audio->device_mutex);
  *info = {};
  const auto copy = [](char *destination, const std::string &source) {
    std::strncpy(destination, source.c_str(), KSIP_AUDIO_DEVICE_TEXT_SIZE - 1);
  };
  const auto side = [&](const std::string &opened, const std::string &name, const std::string &id, char *name_out, char *id_out) {
    const bool moved = !opened.empty() && opened != id;
    copy(name_out, moved ? std::string() : name);
    copy(id_out, moved ? opened : id);
  };
  side(opened_recording, audio->recording_name, audio->recording_id, info->recording_name, info->recording_id);
  side(opened_playout, audio->playout_name, audio->playout_id, info->playout_name, info->playout_id);
  return 0;
}
