"""Verify that the engine watches as many numbers as there are custom buttons at once, and that the PBX answers every one of them with a state."""
from sip_fixture import Phone, accounts, numbers

# One for each custom button (WATCH_COUNT in ksip_text.h, CustomButton::COUNT in settings.rs).
COUNT = 54


def main():
    first = numbers().get('watch_first')
    if not first:
        print('SKIP: 54本の購読に使う番号が無い（lab.json の numbers.watch_first に、続き番号の先頭を書く）', flush=True)
        return
    watched = [str(int(first) + i) for i in range(COUNT)]
    a = None
    try:
        a = Phone('blf-54', accounts()[0])
        a.command('ksip_parking', ','.join(watched))
        # Every subscription is answered with a state; none is left unknown.
        state = a.wait(lambda s: len(s['parking']) == COUNT and all(p['state'] != 'UNKNOWN' for p in s['parking']), timeout=30)
        got = [p['number'] for p in state['parking']]
        assert got == watched, f'watched numbers differ: {got[:3]}...'
        states = sorted({p['state'] for p in state['parking']})
        print(f'PASS: {COUNT}本の購読すべてに状態が届いた（{watched[0]}〜{watched[-1]}、状態: {", ".join(states)}）')
        # Still registered after the burst: the PBX has not shut the phone out.
        a.wait(lambda s: s.get('registration') == 'REGISTER_OK', timeout=10)
        print('PASS: 購読の後も登録が保たれている')
    finally:
        if a:
            a.close()


if __name__ == '__main__':
    main()
