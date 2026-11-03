"""Exercise microphone and speaker gain over real local G.711/RTP calls: 200% and 300% (the most the app allows) against 100%, and 301% refused."""
from pathlib import Path
import importlib.util, json

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location("ksip_call_test", ROOT / "scripts/test/loopback-call.py")
call_test = importlib.util.module_from_spec(spec)
spec.loader.exec_module(call_test)
call_test.BASE = ROOT / "temp/build/software-gain-test"
call_test.BASE.mkdir(parents=True, exist_ok=True)
# The source tone peaks at 8000, so 300% (24000) is not clipped and stays a
# clean three times as loud.
LEVELS = (200, 300)


def establish(receiver, sender):
    sender.command("dial", f"sip:{receiver.name}@127.0.0.1:{receiver.sip}")
    receiver.event("CALL_INCOMING")
    receiver.command("accept")
    receiver.event("CALL_ESTABLISHED")
    sender.event("CALL_ESTABLISHED")


def record(phone, name):
    import time
    path = phone.dir / name
    phone.command("ksip_record", str(path))
    time.sleep(5)
    phone.command("ksip_record_stop")
    return path


def refused(phone, params):
    """Whether the engine turns ksip_gain down, as it answers (not a timeout)."""
    try:
        phone.command("ksip_gain", params)
    except AssertionError as e:
        return "'ok': False" in str(e)
    return False


def main():
    # Every port is one the system picks (0): fixed ones can fall in a range
    # Windows has reserved since the last boot.
    phones = []
    try:
        receiver = call_test.Phone("receiver", 0, 0, 0, False, False)
        phones.append(receiver)
        sender = call_test.Phone("sender", 0, 0, 0, False, True, True)
        phones.append(sender)
        establish(receiver, sender)
        microphone = {}
        for level in (100,) + LEVELS:
            sender.command("ksip_gain", f"microphone {level}")
            microphone[level] = call_test.stats(record(receiver, f"gain-{level}.wav"))
        too_much = refused(sender, "microphone 301")
        sender.command("hangup")
        receiver.event("CALL_CLOSED")
    finally:
        for phone in reversed(phones):
            phone.close()
    microphone_ratio = {level: microphone[level]["rms"] / microphone[100]["rms"] for level in LEVELS}
    assert too_much, "301% was taken"
    assert 1.7 < microphone_ratio[200] < 2.2 and 2.6 < microphone_ratio[300] < 3.3, microphone_ratio

    # The speaker's gain is measured on what one call plays against what it
    # received, so each level has a call of its own.
    speaker = {}
    for level in LEVELS:
        phones = []
        try:
            speaker_receiver = call_test.Phone(f"speaker_receiver_{level}", 0, 0, 0, False, False)
            phones.append(speaker_receiver)
            speaker_sender = call_test.Phone(f"speaker_sender_{level}", 0, 0, 0, False, True, True)
            phones.append(speaker_sender)
            establish(speaker_receiver, speaker_sender)
            speaker_receiver.command("ksip_gain", f"speaker {level}")
            speaker_input = record(speaker_receiver, "speaker-input.wav")
            speaker_sender.command("hangup")
            speaker_receiver.event("CALL_CLOSED")
        finally:
            for phone in reversed(phones):
                phone.close()
        before = call_test.stats(speaker_input)
        after = call_test.stats(speaker_receiver.dir / "speaker.wav")
        speaker[level] = {"normal": before, "gain": after, "ratio": after["rms"] / before["rms"]}
    assert 1.7 < speaker[200]["ratio"] < 2.2 and 2.6 < speaker[300]["ratio"] < 3.3, {k: v["ratio"] for k, v in speaker.items()}
    result = {
        "microphone": {"levels": microphone, "ratio": microphone_ratio, "refused_301": too_much},
        "speaker": speaker,
    }
    print(json.dumps(result, indent=2))
    print("PASS: runtime microphone and speaker software gain at 200% and 300%, and 301% refused")


if __name__ == "__main__":
    main()
