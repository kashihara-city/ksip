// Which endpoint the ADM opens for a request: the Windows default
// communications device, or the endpoint whose id was asked for, matched
// among everything WebRTC lists (the default in its place while it is not
// there); and the name and id of what was opened, for the app to show and
// for the microphone mute to read.
#include "bridge_state.h"

#include <cstring>

#include "rtc_base/logging.h"

using ksip_audio_bridge::kRoleEntries;

int ksip_audio::SelectDefault(bool playout) {
  const int result = playout
      ? adm->SetPlayoutDevice(webrtc::AudioDeviceModule::kDefaultCommunicationDevice)
      : adm->SetRecordingDevice(webrtc::AudioDeviceModule::kDefaultCommunicationDevice);
  if (result) return result;
  // The name and id are those of the list's communications role entry
  // (kCommunicationsEntry: core_audio_utility_win.cc). Asked for at index -1
  // (the device type's value), the name was never there.
  char name[webrtc::kAdmMaxDeviceNameSize] = {};
  char guid[webrtc::kAdmMaxGuidSize] = {};
  const int name_result = playout
      ? adm->PlayoutDeviceName(ksip_audio_bridge::kCommunicationsEntry, name, guid)
      : adm->RecordingDeviceName(ksip_audio_bridge::kCommunicationsEntry, name, guid);
  if (!name_result) StoreDevice(playout, name, guid);
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
  const int count = playout ? adm->PlayoutDevices() : adm->RecordingDevices();
  const int enumerated = count > 0 ? count + kRoleEntries : 0;
  int role_index = -1;
  std::string listing;
  for (int i = 0; i < enumerated; ++i) {
    char name[webrtc::kAdmMaxDeviceNameSize] = {};
    char guid[webrtc::kAdmMaxGuidSize] = {};
    const int result = playout
        ? adm->PlayoutDeviceName(static_cast<uint16_t>(i), name, guid)
        : adm->RecordingDeviceName(static_cast<uint16_t>(i), name, guid);
    if (result) continue;
    listing += " [" + std::to_string(i) + "] " + name + " " + guid;
    if (std::strcmp(id, guid) != 0) continue;
    if (i < kRoleEntries) {
      if (role_index < 0) role_index = i;
      continue;
    }
    return Select(playout, i, name, guid);
  }
  if (role_index >= 0) {
    char name[webrtc::kAdmMaxDeviceNameSize] = {};
    char guid[webrtc::kAdmMaxGuidSize] = {};
    const int result = playout
        ? adm->PlayoutDeviceName(static_cast<uint16_t>(role_index), name, guid)
        : adm->RecordingDeviceName(static_cast<uint16_t>(role_index), name, guid);
    if (!result) return Select(playout, role_index, name, guid);
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
  const int count = playout ? adm->PlayoutDevices() : adm->RecordingDevices();
  const int enumerated = count > 0 ? count + kRoleEntries : 0;
  for (int i = 0; i < enumerated; ++i) {
    char name[webrtc::kAdmMaxDeviceNameSize] = {};
    char guid[webrtc::kAdmMaxGuidSize] = {};
    const int result = playout
        ? adm->PlayoutDeviceName(static_cast<uint16_t>(i), name, guid)
        : adm->RecordingDeviceName(static_cast<uint16_t>(i), name, guid);
    if (!result && std::strcmp(id, guid) == 0) return true;
  }
  return false;
}

// Whether a stream running for `request` serves it as one opened for it now
// would: on the endpoint asked for, or on the default because that one is
// not there, whether the stream opened there in its place (SetDevice) or
// WebRTC moved it there when the device in use went away (the endpoint
// WebRTC says it opened tells which, OpenedEndpoint). "default" is WebRTC's
// to follow. A stream on the default does not serve a device that is back.
bool ksip_audio::Serves(const std::string &request, bool playout) {
  if (request == "default") return true;
  if (ksip_audio_bridge::OpenedEndpoint(playout) == request) return true;
  if (!Listed(request.c_str(), playout)) return true;
  RTC_LOG(LS_WARNING) << "ksip_audio: " << (playout ? "playout" : "recording") << " device " << request
                      << " is back, the stream is opened on it again";
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
