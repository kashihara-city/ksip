"""Verify the local Asterisk 9001 auto-answer RTP audio source."""
from array import array
import json
import math
import time
import wave

from sip_fixture import ROOT, Phone, accounts


def main():
    base = ROOT / "temp/build/asterisk-playback-test"
    base.mkdir(parents=True, exist_ok=True)
    received = base / "received.wav"
    received.unlink(missing_ok=True)
    phone = Phone(
        "asterisk-playback",
        accounts()[0],
        17660,
        17900,
        f"aufile,{received.as_posix()}",
    )
    try:
        call_id = phone.action("dial", value="9001")
        phone.wait(
            lambda state: any(
                call["id"] == call_id and call["state"] == "ESTABLISHED"
                for call in state["calls"]
            ),
            timeout=20,
        )
        time.sleep(8)
        phone.action("hangup", call_id)
        phone.wait(lambda state: not state["calls"], timeout=10)
    finally:
        phone.close()

    with wave.open(str(received), "rb") as wav:
        assert wav.getnchannels() == 1
        assert wav.getsampwidth() == 2
        assert wav.getframerate() == 48000
        samples = array("h", wav.readframes(wav.getnframes()))
    assert len(samples) >= 48000 * 6, len(samples)
    rms = math.sqrt(sum(sample * sample for sample in samples) / len(samples))
    peak = max(abs(sample) for sample in samples)
    assert rms > 100 and peak > 1000, (rms, peak)
    result = {
        "passed": True,
        "extension": "9001",
        "sample_rate": 48000,
        "samples": len(samples),
        "seconds": round(len(samples) / 48000, 2),
        "rms": round(rms, 2),
        "peak": peak,
    }
    (ROOT / "temp/reports/asterisk-playback-9001.json").write_text(
        json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    print(
        f"PASS: Asterisk 9001 auto-answer audio, {result['seconds']} s, "
        f"RMS {result['rms']}, peak {peak}"
    )


if __name__ == "__main__":
    main()
