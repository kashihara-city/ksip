"""Apply reviewed KSIP adapters to pinned pristine re/baresip sources."""
# What is changed, and why. Each file is first restored from the pinned
# archive, so running this twice gives the same result.
#
# Behaviour of the SIP engine:
#   re/src/sipevent/subscribe.c   an un-SUBSCRIBE is over with its 2xx; libre
#                                 would otherwise hold the SIP stack up to ten
#                                 seconds for a terminating NOTIFY that some
#                                 PBXs never send, and quitting took that long.
#   re/src/rtp/rtp.c              "rtp_ports 0", which libre refused, puts RTP
#                                 and RTCP on ports the system picks (no
#                                 listener the Windows Firewall asks about).
#                                 A port range takes the code it always did.
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
# Nothing else in re or baresip is touched; baresip itself is as released.
# What the system-picked ports need besides (an empty datagram that opens the
# way back for a DTLS handshake the peer starts) is the ksip module's
# (system_ports.cpp), for those calls alone. The SRTP module in particular is
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
restore("re", re_base, ["cmake/re-config.cmake", "src/sipevent/subscribe.c", "src/rtp/rtp.c"])

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

# "rtp_ports 0": RTP and RTCP on ports the system picks. A socket bound to a
# port of its own choosing is a listener to the Windows Firewall, which then
# asks whether to allow the program on public and private networks; one the
# system gives a port to is not, and the replies to what it sends come back
# through the firewall's state. libre refused a range of 0-0 (EINVAL), so the
# new path runs for that setting alone; a range takes the same code as before.
# The pair is the usual one, an even RTP port and RTCP on the next (RFC 3550
# section 11), which the SDP implies without saying it: a peer sends RTCP
# there whatever a=rtcp says (Asterisk does). The system hands ports out
# nearly in turn, so a second socket lands next to the first; when it does
# not, it becomes the RTP candidate and another is asked for. Should no pair
# come of it, the last two are kept: the audio goes, the peer's RTCP may not
# arrive (the ksip module warns of it).
rtp = re_base / "src/rtp/rtp.c"
text = rtp.read_text()
range_listen = "static int udp_range_listen(struct rtp_sock *rs, const struct sa *ip,\n"
check = "\tif (!ip || min_port >= max_port || !recvh)\n"
listen = "\t\terr = udp_range_listen(rs, ip, min_port, max_port);\n"
if text.count(range_listen) != 1 or text.count(check) != 1 or text.count(listen) != 1:
    raise SystemExit("patch-baresip: rtp_listen in rtp.c has changed")
text = text.replace(range_listen, '''/* KSIP: "rtp_ports 0", RTP and RTCP on ports the system picks: an even
 * RTP port and RTCP on the next, as near as the system allows. */
static int udp_system_listen(struct rtp_sock *rs, const struct sa *ip)
{
\tstruct udp_sock *us_rtp = NULL, *us_rtcp = NULL;
\tstruct sa any = *ip, rtp, rtcp;
\tint tries = 64;
\tint err;

\tsa_set_port(&any, 0);

\terr = udp_listen(&us_rtp, &any, udp_recv_handler, rs);
\tif (!err)
\t\terr = udp_local_get(us_rtp, &rtp);

\twhile (!err) {
\t\terr = udp_listen(&us_rtcp, &any, rtcp_recv_handler, rs);
\t\tif (!err)
\t\t\terr = udp_local_get(us_rtcp, &rtcp);
\t\tif (err)
\t\t\tbreak;
\t\tif ((sa_port(&rtp) % 2 == 0 &&
\t\t     sa_port(&rtcp) == sa_port(&rtp) + 1) || !--tries)
\t\t\tbreak;

\t\t/* the second socket is the next RTP candidate */
\t\tmem_deref(us_rtp);
\t\tus_rtp = us_rtcp;
\t\tus_rtcp = NULL;
\t\trtp = rtcp;
\t\tudp_handler_set(us_rtp, udp_recv_handler, rs);
\t}

\tif (err) {
\t\tmem_deref(us_rtcp);
\t\tmem_deref(us_rtp);
\t\treturn err;
\t}

\trs->local = *ip;
\tsa_set_port(&rs->local, sa_port(&rtp));
\trs->sock_rtp = us_rtp;
\trs->sock_rtcp = us_rtcp;

\treturn 0;
}


''' + range_listen)
text = text.replace(check, "\tif (!ip || (min_port >= max_port && (min_port || max_port)) ||\n\t    !recvh)\n")
text = text.replace(listen, '''\t\tif (!min_port && !max_port)
\t\t\terr = udp_system_listen(rs, ip);
\t\telse
\t\t\terr = udp_range_listen(rs, ip, min_port, max_port);
''')
rtp.write_text(text)

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
# The filter module lives in the same folder as the device module; baresip
# wants a folder per module, so it gets one holding only the CMakeLists.
(baresip / "modules/ksip_audio_filter").mkdir(parents=True, exist_ok=True)
shutil.copy2(ROOT / "src-native/ksip_audio/filter/CMakeLists.txt",
             baresip / "modules/ksip_audio_filter/CMakeLists.txt")
shutil.copytree(ROOT / "src-native/ksip",
                baresip / "modules/ksip", dirs_exist_ok=True)
# The app's authenticated control connection, in place of baresip's ctrl_tcp.
shutil.copytree(ROOT / "src-native/ksip_ctrl",
                baresip / "modules/ksip_ctrl", dirs_exist_ok=True)

cmake = baresip / "CMakeLists.txt"
cmake.write_text(cmake.read_text() + "\n# KSIP embedded entry point\n" +
                 (ROOT / "src-native/embedded.cmake").read_text())
