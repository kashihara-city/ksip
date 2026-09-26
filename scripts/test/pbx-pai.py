"""Verify that a parked-call pickup adopts the PBX's connected identity (PAI)."""
from sip_fixture import Phone, accounts, connect, numbers, park_target, park_watch, skip_unless


def main():
    skip_unless('connected_identity', '取得した通話の表示が PAI に従うのは、PBX が接続先の identity を送るときだけ')
    configured = accounts()
    slots = numbers()['park_slots']
    a = b = None
    try:
        a = Phone("pai-a", configured[0], 19660, 19700)
        b = Phone("pai-b", configured[1], 19662, 19730)
        a.command("ksip_parking", ",".join(park_watch(s) for s in slots))

        original, _ = connect(a, b, configured[1]["extension"])

        # The lab parks by transferring to the prefixed slot and picks up by dialling the slot.
        a.action("blind_transfer", original, park_target(slots[0]))
        a.wait(lambda s: not s["calls"], timeout=20)
        b.wait(lambda s: any(c["state"] == "ESTABLISHED" for c in s["calls"]))

        pickup = a.action("dial", value=slots[0])
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
