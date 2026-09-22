"""Verify that an unavailable WASAPI microphone still produces timed RTP."""
from pathlib import Path
import array, json, math, socket, subprocess, threading, time, wave

ROOT = Path(__file__).resolve().parents[2]
BASE = ROOT / "temp" / "build" / "silent-fallback-test"
BASE.mkdir(parents=True, exist_ok=True)


def source(path):
    samples = array.array("h", (0 for _ in range(48000 * 15)))
    with wave.open(str(path), "wb") as output:
        output.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
        output.writeframes(samples.tobytes())


class Phone:
    def __init__(self, name, sip, ctrl, rtp, microphone):
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
audio_alert aufile,NUL
ausrc_srate 48000
auplay_srate 48000
ausrc_channels 1
auplay_channels 1
ausrc_format s16
auplay_format s16
auenc_format s16
audec_format s16
rtp_ports {rtp}-{rtp + 20}
ctrl_tcp_listen 127.0.0.1:{ctrl}
module g711.dll
module ksip_audio.dll
module aufile.dll
module postlab.dll
module auconv.dll
module auresamp.dll
module ctrl_tcp.dll
module menu.dll
module_app account.dll
"""
        (self.directory / "config").write_text(config, encoding="utf-8")
        (self.directory / "accounts").write_text(
            f"<sip:{name}@localhost:{sip};transport=udp>;regint=0;audio_codecs=PCMU/8000/1;answermode=manual\n",
            encoding="utf-8",
        )
        self.log = open(self.directory / "engine.log", "wb")
        self.process = subprocess.Popen(
            [str(ROOT / "temp/build/native/bin/baresip.exe"), "-f", str(self.directory)],
            cwd=self.directory,
            stdin=subprocess.DEVNULL,
            stdout=self.log,
            stderr=subprocess.STDOUT,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        deadline = time.monotonic() + 10
        while True:
            try:
                self.socket = socket.create_connection(("127.0.0.1", ctrl), 0.3)
                break
            except OSError:
                if self.process.poll() is not None or time.monotonic() > deadline:
                    raise RuntimeError(f"{name} startup failed")
                time.sleep(0.1)
        self.socket.settimeout(None)
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
        receiver = Phone("receiver", 17060, 17444, 18000, "aufile,source.wav")
        phones.append(receiver)
        sender = Phone("sender", 17062, 17445, 18100, "ksip_audio,{KSIP-NO-SUCH-DEVICE}")
        phones.append(sender)
        sender.command("dial", "sip:receiver@127.0.0.1:17060")
        receiver.event("CALL_INCOMING")
        receiver.command("accept")
        receiver.event("CALL_ESTABLISHED")
        sender.event("CALL_ESTABLISHED")
        receiver.command("lab_record", str(receiver.directory / "received.wav"))
        time.sleep(6)
        receiver.command("lab_stop")
        sender.command("hangup")
        receiver.event("CALL_CLOSED")
    finally:
        for phone in reversed(phones):
            phone.close()

    with wave.open(str(receiver.directory / "received.wav"), "rb") as recording:
        rate = recording.getframerate()
        samples = array.array("h", recording.readframes(recording.getnframes()))
    rms = math.sqrt(sum(value * value for value in samples) / max(1, len(samples)))
    log = (sender.directory / "engine.log").read_text(encoding="utf-8", errors="replace")
    assert len(samples) > rate * 3, (len(samples), rate)
    # G.711 μ-law's representation of digital silence decodes to about one
    # signed PCM unit, rather than necessarily to exactly zero.
    assert rms < 2, rms
    assert "ksip: microphone fallback active" in log, log
    print(json.dumps({"samples": len(samples), "rate": rate, "rms": rms}, indent=2))
    print("PASS: unavailable WebRTC ADM microphone produced continuous silent RTP")


if __name__ == "__main__":
    main()
