"""Verify that a parked-call pickup adopts Asterisk's connected PAI."""
from sip_fixture import Phone, accounts, connect


def main():
    configured = accounts()
    a = b = None
    try:
        a = Phone("pai-a", configured[0], 19660, 19700)
        b = Phone("pai-b", configured[1], 19662, 19730)
        a.command("ksip_parking", "701,702,703")

        original, _ = connect(a, b, configured[1]["extension"])

        a.action("park", original, "701")
        a.wait(lambda s: not s["calls"], timeout=20)
        b.wait(lambda s: any(c["state"] == "ESTABLISHED" for c in s["calls"]))

        pickup = a.action("dial", value="701")
        state = a.wait(
            lambda s: any(
                c["id"] == pickup
                and c["state"] == "ESTABLISHED"
                and c["peer"].startswith("sip:" + configured[1]["extension"] + "@")
                for c in s["calls"]
            ),
            timeout=20,
        )
        peer = next(c["peer"] for c in state["calls"] if c["id"] == pickup)
        # The host part is the lab's address, which stays out of any log.
        print("PASS: parked pickup display follows received PAI (peer=" + peer.split("@")[0] + ")")
    finally:
        if a:
            a.close()
        if b:
            b.close()


if __name__ == "__main__":
    main()
