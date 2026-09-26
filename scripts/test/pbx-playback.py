"""Verify the lab PBX's auto-answer playback number as an RTP audio source."""
from array import array
import json
import math
import time
import wave

from sip_fixture import PBX, ROOT, Phone, accounts, numbers


def main():
    base = ROOT / "temp/build/pbx-playback-test"
    base.mkdir(parents=True, exist_ok=True)
    received = base / "received.wav"
    received.unlink(missing_ok=True)
    phone = Phone(
        "pbx-playback",
        accounts()[0],
        17660,
        17900,
        f"aufile,{received.as_posix()}",
    )
    try:
        playback = numbers()["playback"]
        call_id = phone.action("dial", value=playback)
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

    # The player fixture writes stereo like a call recording (far end on the
    # left); the first channel is read either way.
    with wave.open(str(received), "rb") as wav:
        assert wav.getnchannels() in (1, 2)
        assert wav.getsampwidth() == 2
        assert wav.getframerate() == 48000
        samples = array("h", wav.readframes(wav.getnframes()))[0::wav.getnchannels()]
    assert len(samples) >= 48000 * 6, len(samples)
    rms = math.sqrt(sum(sample * sample for sample in samples) / len(samples))
    peak = max(abs(sample) for sample in samples)
    assert rms > 100 and peak > 1000, (rms, peak)
    result = {
        "passed": True,
        "extension": playback,
        "sample_rate": 48000,
        "samples": len(samples),
        "seconds": round(len(samples) / 48000, 2),
        "rms": round(rms, 2),
        "peak": peak,
    }
    (ROOT / f"temp/reports/pbx-playback-{PBX}.json").write_text(
        json.dumps(result, ensure_ascii=False, indent=2), encoding="utf-8"
    )
    print(
        f"PASS: PBX {playback} auto-answer audio, {result['seconds']} s, "
        f"RMS {result['rms']}, peak {peak}"
    )


if __name__ == "__main__":
    main()
