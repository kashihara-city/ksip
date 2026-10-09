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
#   baresip/src/stream.c          the SDP says where RTCP is (a=rtcp, RFC
#                                 3605) when it is not on the port next to
#                                 RTP, which only the system-picked ports
#                                 leave possible. And, with net_interface
#                                 set, the RTP socket is bound to that
#                                 interface's address, not to every
#                                 interface: the RTP then leaves from the
#                                 address the SDP names (RFC 4961), not from
#                                 whichever interface the routing table
#                                 prefers.
#   baresip/src/ua.c              an outgoing call with net_interface set,
#                                 when the routing table prefers another
#                                 interface, goes from the address the
#                                 registration goes from (the interface's)
#                                 instead of being refused ("no laddr").
#
# Building and embedding (no change to what goes on the wire):
#   baresip/src/main.c            take only "-f PROFILE_DIRECTORY" on the
#                                 command line, since MSVC has no getopt.
#   baresip/CMakeLists.txt        append the embedded entry point (baresip's
#                                 main under another name). The KSIP modules
#                                 are built apart (src-native/CMakeLists.txt,
#                                 with clang-cl); baresip gets a stub per
#                                 module below, so that its static module
#                                 table names them.
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
restore("re", re_base, ["src/sipevent/subscribe.c", "src/rtp/rtp.c"])

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
# come of it, the last two are kept: the audio goes, and the SDP tells the
# peer where RTCP is (the stream.c change below; the ksip module warns of
# it, since a peer that ignores a=rtcp sends its RTCP next to RTP).
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
restore("baresip", baresip, ["src/main.c", "src/stream.c", "src/ua.c", "CMakeLists.txt"])

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

# On the wire: RTCP on another port than the next after RTP (the last pair
# of udp_system_listen's search) is said in the SDP, so that a peer that
# reads a=rtcp sends its RTCP where it is listened for. The usual pair says
# nothing, as before.
stream = baresip / "src/stream.c"
text = stream.read_text()
anchor = """	/* RFC 5761 */
	if (s->cfg.rtcp_mux &&
	    (offerer || sdp_media_rattr(s->sdp, "rtcp-mux"))) {
"""
if text.count(anchor) != 1:
    raise SystemExit("patch-baresip: stream_alloc in stream.c has changed")
text = text.replace(anchor, """	/* KSIP: RFC 3605, when RTCP is not on the port next to RTP (the
	 * system-picked ports, with no such pair to be had) */
	if (s->rtp && rtcp_sock(s->rtp)) {
		struct sa rtcp_local;
		if (!udp_local_get(rtcp_sock(s->rtp), &rtcp_local) &&
		    sa_port(&rtcp_local) != sa_port(rtp_local(s->rtp)) + 1)
			err |= sdp_media_set_lattr(s->sdp, true, "rtcp", "%u",
						   sa_port(&rtcp_local));
	}

""" + anchor)
# On the wire: with net_interface set, the SIP transports and the SDP's
# address are the interface's, but the RTP socket listened on every
# interface, and its packets left by whichever interface the routing table
# preferred: another one, with another source address, when a wired and a
# wireless interface share a LAN. A PBX that matches the source against the
# SDP then hears nothing, and one that follows the source moves the audio
# onto the interface not chosen. Bound to the interface's address, the RTP
# leaves from the address the SDP names (RFC 4961). Without net_interface
# nothing changes: every interface, as baresip has it.
any_address = "\t/* we listen on all interfaces */\n\tsa_init(&laddr, af);\n"
if text.count(any_address) != 1:
    raise SystemExit("patch-baresip: stream_sock_alloc in stream.c has changed")
text = text.replace(any_address, """\t/* we listen on all interfaces; or, with net_interface set, on that
\t * interface's address alone, so that the RTP leaves from the address
\t * the SDP names (KSIP) */
\tsa_init(&laddr, af);
\tif (str_isset(conf_config()->net.ifname)) {
\t\tconst struct sa *ifaddr = net_laddr_af(baresip_network(), af);
\t\tif (ifaddr && sa_isset(ifaddr, SA_ADDR))
\t\t\tsa_cpy(&laddr, ifaddr);
\t}
""")
stream.write_text(text)

# On the wire: with net_interface set, an outgoing call is made from that
# interface's address. baresip picks a call's local address by the routing
# table's source address toward the peer and refuses the call ("no laddr")
# when that address is not the interface's: a wired and a wireless interface
# on one LAN, with Windows preferring the wireless one, made every call fail
# while the registration (libre falls back to the transport it has) went
# through. The address the registration goes from is taken instead, as an
# older baresip (the one tSIP builds on) always did: the SIP, its SDP and the
# RTP (bound above) then all leave by the chosen interface, which Windows'
# strong host model sends them by. With the routing table agreeing, or no
# net_interface, nothing changes.
ua = baresip / "src/ua.c"
text = ua.read_text()
no_laddr = """\t\tladdr = net_laddr_for(net, &ua->dst);
\t\tif (!sa_isset(laddr, SA_ADDR)) {
\t\t\twarning("ua: no laddr for %j\\n", &ua->dst);
"""
if text.count(no_laddr) != 1:
    raise SystemExit("patch-baresip: ua_call_alloc in ua.c has changed")
text = text.replace(no_laddr, """\t\tladdr = net_laddr_for(net, &ua->dst);
\t\t/* KSIP: with net_interface set and the routing table preferring
\t\t * another interface, the address the registration goes from */
\t\tif (!sa_isset(laddr, SA_ADDR) && ua->acc->regint)
\t\t\tladdr = ua_regladdr(ua);
\t\tif (!sa_isset(laddr, SA_ADDR)) {
\t\t\twarning("ua: no laddr for %j\\n", &ua->dst);
""")
ua.write_text(text)

# Embedding: the KSIP modules are built apart from baresip, with clang-cl,
# into one archive (src-native/CMakeLists.txt, native.ps1). baresip gets a
# stub per module, with no sources of its own, so that its static module
# table names each one (exports_<name>) and the archive is linked where the
# module's objects would be, with what the modules need besides (the WebRTC
# bridge and its builtins, the Windows libraries). A copy of the sources an
# earlier version of this script made is removed first.
needed = ";".join([
    "${KSIP_NATIVE}/lib/ksip_webrtc_audio.lib",
    "${KSIP_NATIVE}/lib/clang_rt.builtins-x86_64.lib",
    "winmm", "crypt32", "iphlpapi", "secur32", "oleaut32", "ole32", "uuid", "advapi32",
])
for name in ("ksip_audio", "ksip_audio_filter", "ksip", "ksip_ctrl"):
    folder = baresip / "modules" / name
    shutil.rmtree(folder, ignore_errors=True)
    folder.mkdir(parents=True)
    (folder / "CMakeLists.txt").write_text(
        "# KSIP: a module built apart (src-native/CMakeLists.txt); only named here.\n"
        f"project({name})\n"
        "list(APPEND MODULES_DETECTED ${PROJECT_NAME})\n"
        "set(MODULES_DETECTED ${MODULES_DETECTED} PARENT_SCOPE)\n"
        "add_library(${PROJECT_NAME} STATIC IMPORTED GLOBAL)\n"
        "set_target_properties(${PROJECT_NAME} PROPERTIES\n"
        '  IMPORTED_LOCATION "${KSIP_NATIVE}/lib/ksip_modules.lib"\n'
        f'  INTERFACE_LINK_LIBRARIES "{needed}")\n')

# The embedded entry point: baresip's main, under another name, for the app
# to call when it runs as the engine (src-tauri/src/native.rs).
cmake = baresip / "CMakeLists.txt"
cmake.write_text(cmake.read_text() + """
# KSIP embedded entry point
add_library(ksip_entry STATIC src/main.c)
target_compile_definitions(ksip_entry PRIVATE main=ksip_engine_main)
target_link_libraries(ksip_entry PRIVATE baresip)
install(TARGETS ksip_entry ARCHIVE DESTINATION ${CMAKE_INSTALL_LIBDIR})
""")
