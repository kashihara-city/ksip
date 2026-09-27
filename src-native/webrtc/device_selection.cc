// Which endpoint the ADM opens for a request: the Windows default
// communications device, or the endpoint whose id was asked for, matched
// among everything WebRTC lists; and the name and id of what was chosen,
// for the app to show.
#include "bridge_state.h"

#include <cstring>

#include "rtc_base/logging.h"

using ksip_audio_bridge::kRoleEntries;

int ksip_audio::SetDevice(const char *id, bool playout) {
  if (!id || !id[0] || std::strcmp(id, "default") == 0) {
    const int result = playout
        ? adm->SetPlayoutDevice(webrtc::AudioDeviceModule::kDefaultCommunicationDevice)
        : adm->SetRecordingDevice(webrtc::AudioDeviceModule::kDefaultCommunicationDevice);
    if (result) return result;
    char name[webrtc::kAdmMaxDeviceNameSize] = {};
    char guid[webrtc::kAdmMaxGuidSize] = {};
    const int name_result = playout
        ? adm->PlayoutDeviceName(static_cast<uint16_t>(-1), name, guid)
        : adm->RecordingDeviceName(static_cast<uint16_t>(-1), name, guid);
    if (!name_result) StoreDevice(playout, name, guid);
    return 0;
  }
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
  // Written at warning level, which the app's log always carries, so that a
  // device the engine cannot find is explained next to the failure.
  RTC_LOG(LS_WARNING) << "ksip_audio: " << (playout ? "playout" : "recording")
                      << " device " << id << " is not among the " << count
                      << " endpoints WebRTC lists:" << listing;
  return -5;
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
  std::lock_guard<std::mutex> lock(audio->device_mutex);
  *info = {};
  const auto copy = [](char *destination, const std::string &source) {
    std::strncpy(destination, source.c_str(), KSIP_AUDIO_DEVICE_TEXT_SIZE - 1);
  };
  copy(info->recording_name, audio->recording_name);
  copy(info->recording_id, audio->recording_id);
  copy(info->playout_name, audio->playout_name);
  copy(info->playout_id, audio->playout_id);
  return 0;
}
