"""Apply reviewed KSIP adapters to pinned pristine re/baresip sources."""
# What is changed, and why. Each file is first restored from the pinned
# archive, so running this twice gives the same result.
#
# Behaviour of the SIP engine (the only change of that kind):
#   re/src/sipevent/subscribe.c   an un-SUBSCRIBE is over with its 2xx; libre
#                                 would otherwise hold the SIP stack up to ten
#                                 seconds for a terminating NOTIFY that some
#                                 PBXs never send, and quitting took that long.
#
# Building and embedding (no change to what goes on the wire):
#   re/cmake/re-config.cmake      empty the OpenSSL cache variables when neither
#                                 OpenSSL nor mbedTLS is used (LibreSSL is).
#   baresip/src/main.c            take only "-f PROFILE_DIRECTORY" on the
#                                 command line, since MSVC has no getopt.
#   baresip/CMakeLists.txt        append src-native/embedded.cmake, which adds
#                                 the KSIP modules copied in below and the
#                                 embedded entry point.
#
# Nothing else in re or baresip is touched. The SRTP module in particular is
# pristine: KSIP's osrtp / sdes / dtls map to baresip's own srtp / srtp-mand /
# dtls_srtp modes.
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

# Build: libre's CMake leaves the OpenSSL variables unset when TLS comes from
# LibreSSL, and a later reference to them then fails.
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
# terminating NOTIFY. A PBX that never sends one (MOT-PBX does not) therefore
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
restore("baresip", baresip, ["src/main.c", "CMakeLists.txt"])

# Build: baresip's main() parses its options with getopt, which MSVC lacks.
# The engine is only ever started by the app as "baresip -f <profile>", so
# that one form is read by hand and anything else is refused.
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

# Embedding: the KSIP modules live in this repository and are copied beside
# baresip's own modules, and embedded.cmake adds them to the build.
shutil.copytree(ROOT / "src-native/ksip_audio",
                baresip / "modules/ksip_audio", dirs_exist_ok=True)
shutil.copytree(ROOT / "src-native/postlab",
                baresip / "modules/postlab", dirs_exist_ok=True)
shutil.copytree(ROOT / "src-native/ksip",
                baresip / "modules/ksip", dirs_exist_ok=True)

cmake = baresip / "CMakeLists.txt"
cmake.write_text(cmake.read_text() + "\n# KSIP embedded entry point\n" +
                 (ROOT / "src-native/embedded.cmake").read_text())
