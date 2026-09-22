"""Exercise microphone and speaker gain over real local G.711/RTP calls."""
from pathlib import Path
import importlib.util, json

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("ksip_call_test", ROOT / "scripts/test/loopback-call.py")
call_test = importlib.util.module_from_spec(spec)
spec.loader.exec_module(call_test)
call_test.BASE = ROOT / "temp/build/software-gain-test"
call_test.BASE.mkdir(parents=True, exist_ok=True)


def establish(receiver, sender, port):
    sender.command("dial", f"sip:{receiver.name}@127.0.0.1:{port}")
    receiver.event("CALL_INCOMING")
    receiver.command("accept")
    receiver.event("CALL_ESTABLISHED")
    sender.event("CALL_ESTABLISHED")


def main():
    import time
    phones = []
    try:
        receiver = call_test.Phone("receiver", 19060, 19444, 20000, False, False)
        phones.append(receiver)
        sender = call_test.Phone("sender", 19062, 19445, 20100, False, True, True)
        phones.append(sender)
        establish(receiver, sender, 19060)

        sender.command("lab_gain", "microphone 100")
        base = receiver.dir / "gain-100.wav"
        receiver.command("lab_record", str(base))
        time.sleep(5)
        receiver.command("lab_stop")

        sender.command("lab_gain", "microphone 200")
        boosted = receiver.dir / "gain-200.wav"
        receiver.command("lab_record", str(boosted))
        time.sleep(5)
        receiver.command("lab_stop")
        sender.command("hangup")
        receiver.event("CALL_CLOSED")
    finally:
        for phone in reversed(phones):
            phone.close()

    normal = call_test.stats(base)
    gain = call_test.stats(boosted)
    microphone_ratio = gain["rms"] / normal["rms"]
    assert 1.7 < microphone_ratio < 2.2

    phones = []
    try:
        speaker_receiver = call_test.Phone(
            "speaker_receiver", 19260, 19644, 20200, False, False
        )
        phones.append(speaker_receiver)
        speaker_sender = call_test.Phone(
            "speaker_sender", 19262, 19645, 20300, False, True, True
        )
        phones.append(speaker_sender)
        establish(speaker_receiver, speaker_sender, 19260)
        speaker_receiver.command("lab_gain", "speaker 200")
        speaker_input = speaker_receiver.dir / "speaker-input.wav"
        speaker_receiver.command("lab_record", str(speaker_input))
        time.sleep(5)
        speaker_receiver.command("lab_stop")
        speaker_sender.command("hangup")
        speaker_receiver.event("CALL_CLOSED")
    finally:
        for phone in reversed(phones):
            phone.close()

    speaker_before = call_test.stats(speaker_input)
    speaker_after = call_test.stats(speaker_receiver.dir / "speaker.wav")
    speaker_ratio = speaker_after["rms"] / speaker_before["rms"]
    assert 1.7 < speaker_ratio < 2.2
    result = {
        "microphone": {"normal": normal, "gain": gain, "ratio": microphone_ratio},
        "speaker": {"normal": speaker_before, "gain": speaker_after, "ratio": speaker_ratio},
    }
    print(json.dumps(result, indent=2))
    print("PASS: runtime microphone and speaker software gain")


if __name__ == "__main__":
    main()
