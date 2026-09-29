# The WebRTC tests that open real audio devices: aec.ps1 with KSIP_TEST_AUDIO_DEVICE set. Run it after updating WebRTC or changing scripts/build/patch-webrtc.py, on a PC with a microphone and a speaker (KSIP_TEST_AUDIO_MIC names the microphone; default the Windows default).
$ErrorActionPreference = 'Stop'
$env:KSIP_TEST_AUDIO_DEVICE = '1'
& "$PSScriptRoot/aec.ps1"
