#ifndef KSIP_AUDIO_BRIDGE_H_
#define KSIP_AUDIO_BRIDGE_H_

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct ksip_audio ksip_audio;
typedef int (*ksip_audio_render_cb)(void *, int16_t *, size_t);
typedef void (*ksip_audio_capture_cb)(void *, const int16_t *, size_t,
                                      int64_t);

typedef struct ksip_audio_stats {
  double echo_return_loss;
  double echo_return_loss_enhancement;
  double divergent_filter_fraction;
  double residual_echo_likelihood;
  double residual_echo_likelihood_recent_max;
  double render_rms_dbfs;
  double capture_device_rms_dbfs;
  double capture_mono_rms_dbfs;
  double capture_input_rms_dbfs;
  double capture_output_rms_dbfs;
  int32_t delay_ms;
  int32_t delay_median_ms;
  int32_t delay_standard_deviation_ms;
  uint32_t stream_delay_ms;
  uint32_t stream_delay_from_device;
  uint64_t render_frames;
  uint64_t capture_frames;
  uint32_t render_errors;
  uint32_t capture_errors;
  uint32_t capture_device_rate;
  uint32_t capture_device_channels;
  uint32_t flags;
} ksip_audio_stats;

enum { KSIP_AUDIO_DEVICE_TEXT_SIZE = 256 };
typedef struct ksip_audio_device_info {
  char recording_name[KSIP_AUDIO_DEVICE_TEXT_SIZE];
  char recording_id[KSIP_AUDIO_DEVICE_TEXT_SIZE];
  char playout_name[KSIP_AUDIO_DEVICE_TEXT_SIZE];
  char playout_id[KSIP_AUDIO_DEVICE_TEXT_SIZE];
} ksip_audio_device_info;

enum {
  KSIP_AUDIO_STATS_ECHO_RETURN_LOSS = 1u << 0,
  KSIP_AUDIO_STATS_ECHO_RETURN_LOSS_ENHANCEMENT = 1u << 1,
  KSIP_AUDIO_STATS_DELAY = 1u << 2,
  KSIP_AUDIO_STATS_DIVERGENT_FILTER_FRACTION = 1u << 3,
  KSIP_AUDIO_STATS_DELAY_MEDIAN = 1u << 4,
  KSIP_AUDIO_STATS_DELAY_STANDARD_DEVIATION = 1u << 5,
  KSIP_AUDIO_STATS_RESIDUAL_ECHO_LIKELIHOOD = 1u << 6,
  KSIP_AUDIO_STATS_RESIDUAL_ECHO_LIKELIHOOD_RECENT_MAX = 1u << 7,
  KSIP_AUDIO_STATS_RENDER_LEVEL = 1u << 8,
  KSIP_AUDIO_STATS_CAPTURE_INPUT_LEVEL = 1u << 9,
  KSIP_AUDIO_STATS_CAPTURE_OUTPUT_LEVEL = 1u << 10,
  KSIP_AUDIO_STATS_CAPTURE_DEVICE_LEVEL = 1u << 11,
  KSIP_AUDIO_STATS_CAPTURE_MONO_LEVEL = 1u << 12,
};

/* Call before creating: WebRTC prints from LS_INFO unless told otherwise. */
void ksip_audio_set_log_level(int detail);
int ksip_audio_create(uint32_t fallback_delay_ms, int processing_enabled,
                      ksip_audio **out);
void ksip_audio_destroy(ksip_audio *audio);
/* 0 on success; -1 busy or a bad argument, -2 the device could not be set
   up, -3 it could not be started, -4 the processing could not be reset, -5
   the endpoint is not among the devices WebRTC lists (the log names them). */
int ksip_audio_start_playout(ksip_audio *audio, const char *endpoint_id,
                             ksip_audio_render_cb callback, void *arg);
void ksip_audio_stop_playout(ksip_audio *audio);
int ksip_audio_start_recording(ksip_audio *audio, const char *endpoint_id,
                               ksip_audio_capture_cb callback, void *arg);
void ksip_audio_stop_recording(ksip_audio *audio);
int ksip_audio_get_stats(ksip_audio *audio, ksip_audio_stats *stats);
int ksip_audio_get_device_info(ksip_audio *audio,
                               ksip_audio_device_info *info);
/* Supplied by the baresip ksip_audio module for other in-process modules. */
int ksip_audio_get_current_stats(ksip_audio_stats *stats);

#ifdef __cplusplus
}
#endif
#endif
