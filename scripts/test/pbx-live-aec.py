"""Short real-device/Asterisk call that records native WebRTC APM statistics."""
import json
import math
import time
import winreg

from sip_fixture import ROOT, Phone, accounts


def selected_audio():
    with winreg.OpenKey(winreg.HKEY_CURRENT_USER, r"Software\KashiharaCity\ksip") as key:
        settings = json.loads(winreg.QueryValueEx(key, "Settings")[0])
    microphone = settings["microphone"]
    speaker = settings["speaker"]
    for endpoint in (microphone, speaker):
        assert endpoint and "\n" not in endpoint and "\r" not in endpoint
    return microphone, speaker, int(settings.get("aec_delay_ms", 100))


def finite(value):
    return isinstance(value, (int, float)) and math.isfinite(value)


def main():
    microphone, speaker, delay = selected_audio()
    phone = Phone(
        "live-aec",
        accounts()[0],
        17670,
        17930,
        audio_player=f"ksip_audio,{speaker}",
        audio_source=f"ksip_audio,{microphone}",
        extra_config=(
            "module ksip_audio.dll\n"
            f"webrtc_aec_delay_ms {delay}\n"
            "ksip_aec_enabled yes\n"
            "ksip_microphone_gain 100\n"
            "ksip_speaker_gain 100"
        ),
    )
    samples = []
    try:
        call_id = phone.action("dial", value="9001")
        phone.wait(
            lambda state: any(
                call["id"] == call_id and call["state"] == "ESTABLISHED"
                for call in state["calls"]
            ),
            timeout=20,
        )
        for second in range(1, 16):
            time.sleep(1)
            state = phone.state()
            stats = state.get("audio_processing_stats")
            if stats:
                samples.append({"second": second, **stats})
        phone.action("hangup", call_id)
        phone.wait(lambda state: not state["calls"], timeout=10)
    finally:
        phone.close()

    assert samples, "No WebRTC APM statistics were returned"
    final = samples[-1]
    assert final["render_frames"] >= 500
    assert final["capture_frames"] >= 500
    assert final["render_errors"] == 0
    assert final["capture_errors"] == 0
    assert final["capture_device_rate"] == 48000
    assert final["capture_device_channels"] >= 1
    for name in (
        "render_rms_dbfs",
        "capture_device_rms_dbfs",
        "capture_input_rms_dbfs",
        "capture_output_rms_dbfs",
    ):
        assert finite(final[name]), (name, final.get(name))

    report = {
        "passed": True,
        "extension": "9001",
        "duration_seconds": 15,
        "selected_microphone": microphone,
        "selected_speaker": speaker,
        "samples": samples,
    }
    report_path = ROOT / "temp/reports/live-aec-9001.json"
    report_path.parent.mkdir(parents=True, exist_ok=True)
    report_path.write_text(
        json.dumps(report, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    print(
        "PASS: live 9001 WebRTC APM; "
        f"input {final['capture_input_rms_dbfs']:.2f} dBFS, "
        f"output {final['capture_output_rms_dbfs']:.2f} dBFS, "
        f"frames {final['render_frames']}/{final['capture_frames']}, errors 0/0"
    )


if __name__ == "__main__":
    main()
