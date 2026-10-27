"""Verify where the call's sounds play when the ringtone has a speaker of its own: the ringtone on the one ksip_audio_devices names third (moved there while it rings), and the tone of a call that failed (busy here) on the call's speaker rather than the ringtone's, played by the engine module (failure_tone.h) with baresip's own left out. The ringtone's speaker is one no machine has, so that a sound on it stands in on the default and can be told from one on the call's speaker, which does not. On a PC with no speaker at all nothing can play, and the test says so."""
from pathlib import Path
import array, json, os, subprocess, threading, time, wave
import engine_control

ROOT = Path(__file__).resolve().parents[2]
BASE = ROOT / "temp" / "build" / "call-tones-test"
BASE.mkdir(parents=True, exist_ok=True)
NO_SUCH_SPEAKER = "{KSIP-NO-SUCH-SPEAKER}"


def source(path):
    samples = array.array("h", (0 for _ in range(48000 * 15)))
    with wave.open(str(path), "wb") as output:
        output.setparams((1, 2, 48000, 0, "NONE", "not compressed"))
        output.writeframes(samples.tobytes())


class Phone:
    def __init__(self, name, sip, ctrl, rtp, player, alert, extra=""):
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
audio_source aufile,source.wav
audio_player {player}
audio_alert {alert}
audio_path {(ROOT / "temp/build/native/share/baresip").as_posix()}
ausrc_srate 48000
auplay_srate 48000
ausrc_channels 1
auplay_channels {2 if player.startswith("aufile") else 1}
ausrc_format s16
auplay_format s16
auenc_format s16
audec_format s16
rtp_ports {rtp}-{rtp + 20}
ksip_ctrl_connect 127.0.0.1:{ctrl}
{extra}module g711.dll
module ksip_audio.dll
module aufile.dll
module ksip_audio_filter.dll
module auconv.dll
module auresamp.dll
module ksip_ctrl.dll
module menu.dll
module ksip.dll
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

    def alert(self, until=lambda alert: alert.get("playing") or alert.get("failures"), timeout=5):
        """The alert part of the audio module's state (audio_state.h), once it is as wanted."""
        end = time.monotonic() + timeout
        while True:
            alert = json.loads(self.command("ksip_audio_state"))["audio"].get("alert", {})
            if until(alert) or time.monotonic() > end:
                return alert
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
        # The receiver rings: its ringtone's speaker, the third device, is
        # one no machine has; then the default, while it rings.
        receiver = Phone("receiver", 17260, 17644, 18400, "aufile,NUL", "ksip_alert,default")
        phones.append(receiver)
        receiver.command("ksip_audio_devices", f"source.wav,NUL,{NO_SUCH_SPEAKER}")
        # The caller's call speaker is the default, its ringtone's one no
        # machine has, and the failure tones are the engine module's, as the
        # app's config has them.
        caller = Phone("caller", 17262, 17645, 18500, "ksip_audio,default", f"ksip_alert,{NO_SUCH_SPEAKER}",
                       "busy_aufile none\nnotfound_aufile none\nerror_aufile none\nksip_failure_tones yes\n")
        phones.append(caller)
        caller.command("dial", "sip:receiver@127.0.0.1:17260")
        receiver.event("CALL_INCOMING")
        ringing = receiver.alert()
        if not ringing.get("playing"):
            assert ringing.get("failures", 0) >= 1, ringing
            print("SKIP: no speaker to play on (the alert's start failed as it should)")
            return
        assert ringing["stand_in"] and ringing["failures"] == 0, ("the ringtone on its own speaker, which is not there", ringing)
        receiver.command("ksip_audio_devices", "source.wav,NUL,default")
        moved = receiver.alert(lambda alert: alert.get("playing") and not alert.get("stand_in"))
        assert moved.get("playing") and not moved["stand_in"], ("the ringtone moved onto the default", moved)
        # Busy here: the caller's call fails, and its tone plays on the
        # call's speaker (the default itself), not the ringtone's.
        receiver.command("hangup")
        caller.event("CALL_CLOSED")
        tone = caller.alert()
        closed = [event for event in caller.events if event.get("type") == "CALL_CLOSED"]
        assert any("486" in json.dumps(event, ensure_ascii=False) for event in closed), closed
        assert tone.get("playing") and not tone["stand_in"] and tone["failures"] == 0, ("the busy tone on the call's speaker", tone)
    finally:
        for phone in reversed(phones):
            phone.close()
    log = (caller.directory / "engine.log").read_text(encoding="utf-8", errors="replace")
    assert "the busy.wav tone could not be played" not in log, log[-2000:]
    print("PASS: the ringtone on the speaker named third, moved while it rings; the busy tone on the call's speaker")


if __name__ == "__main__":
    main()
