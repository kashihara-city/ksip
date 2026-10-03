"""Verify that a call whose chosen WASAPI microphone is not there still produces timed RTP: from the Windows default microphone opened in its place when the PC has one, else from silence standing in, with the failed start in the audio module's state; and no input once the call has gone. And that the ringtone of a chosen speaker that is not there goes to the default speaker (ksip_alert) rather than nowhere."""
from pathlib import Path
import array, json, math, os, socket, subprocess, threading, time, wave
import engine_control

ROOT = Path(__file__).resolve().parents[2]
BASE = ROOT / "temp" / "build" / "silent-fallback-test"
BASE.mkdir(parents=True, exist_ok=True)


def source(path):
    samples = array.array("h", (0 for _ in range(48000 * 15)))
    with wave.open(str(path), "wb") as output:
        output.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
        output.writeframes(samples.tobytes())


class Phone:
    def __init__(self, name, sip, ctrl, rtp, microphone, alert="aufile,NUL"):
        self.name, self.responses, self.events, self.serial = name, {}, [], 0
        self.cv = threading.Condition()
        self.directory = BASE / name
        self.directory.mkdir(exist_ok=True)
        source(self.directory / "source.wav")
        config = f"""sip_listen 0.0.0.0:{sip}
net_interface 127.0.0.1
sip_transports udp
sip_cuser_random no
call_max_calls 1
audio_source {microphone}
audio_player aufile,NUL
audio_alert {alert}
audio_path {(ROOT / "temp/build/native/share/baresip").as_posix()}
ausrc_srate 48000
auplay_srate 48000
ausrc_channels 1
auplay_channels 2
ausrc_format s16
auplay_format s16
auenc_format s16
audec_format s16
rtp_ports {rtp}-{rtp + 20}
ksip_ctrl_connect 127.0.0.1:{ctrl}
module g711.dll
module wasapi.dll
module ksip_audio.dll
module aufile.dll
module ksip_audio_filter.dll
module auconv.dll
module auresamp.dll
module ksip_ctrl.dll
module menu.dll
module_app account.dll
"""
        self.secret = engine_control.new_secret()
        control, _ = engine_control.listen(ctrl)
        (self.directory / "config").write_text(config, encoding="utf-8")
        (self.directory / "accounts").write_text(
            f"<sip:{name}@localhost:{sip};transport=udp>;regint=0;audio_codecs=PCMU/8000/1;answermode=manual\n",
            encoding="utf-8",
        )
        self.log = open(self.directory / "engine.log", "wb")
        self.process = subprocess.Popen(
            [str(ROOT / "temp/build/native/bin/baresip.exe"), "-f", str(self.directory)],
            cwd=self.directory,
            env=dict(os.environ, KSIP_CONTROL_SECRET=self.secret),
            stdin=subprocess.DEVNULL,
            stdout=self.log,
            stderr=subprocess.STDOUT,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        try:
            self.socket = engine_control.accept(control, self.process, self.secret)
        except RuntimeError as e:
            raise RuntimeError(f"{name} startup failed ({e})")
        finally:
            control.close()
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        try:
            with self.socket.makefile("rb") as stream:
                while True:
                    header = b""
                    while True:
                        byte = stream.read(1)
                        if not byte:
                            return
                        if byte == b":":
                            break
                        header += byte
                    value = json.loads(stream.read(int(header)))
                    assert stream.read(1) == b","
                    with self.cv:
                        if value.get("event"):
                            self.events.append(value)
                        if "token" in value:
                            self.responses[value["token"]] = value
                        self.cv.notify_all()
        except OSError:
            pass

    def command(self, command, params=""):
        self.serial += 1
        token = str(self.serial)
        data = json.dumps({"command": command, "params": params, "token": token}).encode()
        self.socket.sendall(str(len(data)).encode() + b":" + data + b",")
        with self.cv:
            assert self.cv.wait_for(lambda: token in self.responses, 8)
            value = self.responses.pop(token)
        assert value.get("ok"), value
        return value.get("data", "")

    def audio(self, until=lambda audio: True, timeout=5):
        """The audio module's state (audio_state.h), once it is as wanted."""
        end = time.monotonic() + timeout
        while True:
            audio = json.loads(self.command("ksip_audio_state"))["audio"]
            if until(audio) or time.monotonic() > end:
                return audio
            time.sleep(0.1)

    def event(self, kind):
        with self.cv:
            assert self.cv.wait_for(
                lambda: any(event.get("type") == kind for event in self.events), 10
            ), (self.name, kind, self.events)

    def close(self):
        try:
            if self.process.poll() is None:
                self.command("quit")
        except (OSError, AssertionError):
            pass
        try:
            self.process.wait(timeout=4)
        except subprocess.TimeoutExpired:
            self.process.kill()
            self.process.wait()
        self.socket.close()
        self.log.close()


def main():
    phones = []
    try:
        # The receiver rings on a speaker that no machine has.
        receiver = Phone("receiver", 17060, 17444, 18000, "aufile,source.wav", alert="ksip_alert,{KSIP-NO-SUCH-SPEAKER}")
        phones.append(receiver)
        sender = Phone("sender", 17062, 17445, 18100, "ksip_audio,{KSIP-NO-SUCH-DEVICE}")
        phones.append(sender)
        sender.command("dial", "sip:receiver@127.0.0.1:17060")
        receiver.event("CALL_INCOMING")
        receiver.command("accept")
        receiver.event("CALL_ESTABLISHED")
        sender.event("CALL_ESTABLISHED")
        # The module's own state, not its log: the microphone is silence
        # standing in, and the start that failed is counted as the microphone's.
        during = sender.audio()
        receiver.command("ksip_record", str(receiver.directory / "received.wav"))
        time.sleep(6)
        receiver.command("ksip_record_stop")
        sender.command("hangup")
        receiver.event("CALL_CLOSED")
        after = sender.audio(lambda audio: audio["microphone"]["input"] == "none")
    finally:
        for phone in reversed(phones):
            phone.close()

    with wave.open(str(receiver.directory / "received.wav"), "rb") as recording:
        rate = recording.getframerate()
        samples = array.array("h", recording.readframes(recording.getnframes()))[0 :: recording.getnchannels()]
    rms = math.sqrt(sum(value * value for value in samples) / max(1, len(samples)))
    log = (sender.directory / "engine.log").read_text(encoding="utf-8", errors="replace")
    ringing = (receiver.directory / "engine.log").read_text(encoding="utf-8", errors="replace")
    assert "ksip_alert: speaker {KSIP-NO-SUCH-SPEAKER} is not there" in ringing, ringing
    print("PASS: the ringtone for a speaker that is not there went to the default speaker")
    assert len(samples) > rate * 3, (len(samples), rate)
    if during["microphone"]["input"] == "device":
        # A PC with a microphone: the default one is opened in place of the
        # chosen one, and the call carries what it picks up.
        assert "recording device {KSIP-NO-SUCH-DEVICE} is not among" in log and "in its place" in log, log
        assert during["ready"] and during["microphone"]["failures"] == 0, during
        assert after["microphone"]["input"] == "none", after
        print(json.dumps({"during": during, "after": after}, indent=2))
        print("PASS: the missing WebRTC ADM microphone was replaced by the default one, RTP went on")
        return
    # No microphone at all (a PC without one, a CI runner): silence stands in.
    # G.711 μ-law's representation of digital silence decodes to about one
    # signed PCM unit, rather than necessarily to exactly zero.
    assert rms < 2, rms
    assert "ksip: microphone fallback active" in log, log
    assert during["ready"] and during["microphone"]["input"] == "silence", during
    assert during["microphone"]["failures"] >= 1 and "last_result" in during["microphone"], during
    assert during["speaker"]["failures"] == 0, during
    assert after["microphone"]["input"] == "none" and after["microphone"]["failures"] == during["microphone"]["failures"], after
    print(json.dumps({"during": during, "after": after}, indent=2))
    print(json.dumps({"samples": len(samples), "rate": rate, "rms": rms}, indent=2))
    print("PASS: unavailable WebRTC ADM microphone produced continuous silent RTP")


if __name__ == "__main__":
    main()
