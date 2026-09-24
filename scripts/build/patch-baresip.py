"""Apply reviewed KSIP adapters to pinned pristine re/baresip sources."""
from pathlib import Path
import shutil
import tarfile

ROOT = Path(__file__).resolve().parents[2]


def restore(archive_name: str, base: Path, relative_paths: list[str]):
    with tarfile.open(ROOT / "deps" / f"{archive_name}.tar.gz") as tar:
        for relative in relative_paths:
            member = next(m for m in tar.getmembers()
                          if m.name.endswith("/" + relative))
            (base / relative).write_bytes(tar.extractfile(member).read())


re_base = ROOT / "temp/vendor/re"
restore("re", re_base, ["cmake/re-config.cmake", "src/sipevent/subscribe.c"])
config = re_base / "cmake/re-config.cmake"
text = config.read_text()
anchor = "option(USE_UNIXSOCK"
index = text.index(anchor)
text = text[:index] + '''# KSIP: avoid unresolved optional TLS cache values.
if(NOT USE_OPENSSL AND NOT USE_MBEDTLS)
  set(OPENSSL_INCLUDE_DIR "")
  set(OPENSSL_LIBRARIES "")
  set(LIB_EAY_LIBRARY "")
  set(SSL_EAY_LIBRARY "")
endif()
''' + text[index:]
config.write_text(text)

# After an un-SUBSCRIBE is answered 2xx, libre keeps the subscription, and with
# it a reference to the SIP stack, for up to ten seconds waiting for the
# terminating NOTIFY. A PBX that never sends one (MOIPEX does not) therefore
# kept the engine from quitting until the app ended it, and the window waited
# with it. A subscription the application has already let go needs no final
# state: the 2xx to its un-SUBSCRIBE is the end of it. A NOTIFY that arrives
# later is answered 481 by libre, which is what the notifier expects then.
subscribe = re_base / "src/sipevent/subscribe.c"
text = subscribe.read_text()
wait = '''		if (!sub->expires && !sub->termconf) {

			tmr_start(&sub->tmr, NOTIFY_TIMEOUT,
				  notify_timeout_handler, sub);'''
if text.count(wait) != 1:
    raise SystemExit("patch-baresip: the NOTIFY wait in subscribe.c has changed")
text = text.replace(wait, '''		/* KSIP: a subscription the application dropped is over
		 * with the 2xx; the terminating NOTIFY is not waited for. */
		if (!sub->expires && !sub->termconf && !sub->terminated) {

			tmr_start(&sub->tmr, NOTIFY_TIMEOUT,
				  notify_timeout_handler, sub);''')
subscribe.write_text(text)

baresip = ROOT / "temp/vendor/baresip"
restore("baresip", baresip, ["src/main.c", "CMakeLists.txt", "modules/menu/menu.c"])
main = baresip / "src/main.c"
text = main.read_text()
text = text.replace("err = conf_configure();", '''#ifndef HAVE_GETOPT
    /* Explicit profile argument for the supervised Windows engine. */
    for (int i = 1; i < argc; ++i) {
        if (strcmp(argv[i], "-f") == 0 && i + 1 < argc)
            conf_path_set(argv[++i]);
        else {
            re_fprintf(stderr, "Usage: baresip -f PROFILE_DIRECTORY\\n");
            err = EINVAL;
            goto out;
        }
    }
#endif
    err = conf_configure();''')
main.write_text(text)

# The ringback tone is a notification like the ring tone, not call audio, so
# it goes out the same way: through audio_alert (wasapi on the chosen speaker)
# rather than through the call's player. baresip's menu sends it to the
# player, and ksip_audio only takes the call's 48 kHz mono, so the 8 kHz tone
# was refused (not supported [129]) and the caller heard nothing while a
# server answered 180 Ringing without early media.
menu = baresip / "modules/menu/menu.c"
text = menu.read_text()
ringback = '''		menu_play(call, "ringback_aufile", "ringback.wav", -1,
			  DEVICE_PLAYER);'''
if text.count(ringback) != 1:
    raise SystemExit("patch-baresip: the ringback call in menu.c has changed")
text = text.replace(ringback, '''		menu_play(call, "ringback_aufile", "ringback.wav", -1,
			  DEVICE_ALERT); /* KSIP: alert device, like the ring tone */''')
# The call-waiting tone, played when a second call arrives during a call, went
# the same way and was refused for the same reason. It goes out through the
# alert device too; it sounds beside the call rather than inside it.
callwaiting = '''		menu_play(call, "callwaiting_aufile", "callwaiting.wav", 3,
			  DEVICE_PLAYER);'''
if text.count(callwaiting) != 1:
    raise SystemExit("patch-baresip: the call-waiting call in menu.c has changed")
text = text.replace(callwaiting, '''		menu_play(call, "callwaiting_aufile", "callwaiting.wav", 3,
			  DEVICE_ALERT); /* KSIP: alert device, like the ring tone */''')
menu.write_text(text)

shutil.copytree(ROOT / "src-native/ksip_audio",
                baresip / "modules/ksip_audio", dirs_exist_ok=True)
shutil.copytree(ROOT / "src-native/postlab",
                baresip / "modules/postlab", dirs_exist_ok=True)
shutil.copytree(ROOT / "src-native/ksip",
                baresip / "modules/ksip", dirs_exist_ok=True)

cmake = baresip / "CMakeLists.txt"
cmake.write_text(cmake.read_text() + "\n# KSIP embedded entry point\n" +
                 (ROOT / "src-native/embedded.cmake").read_text())
