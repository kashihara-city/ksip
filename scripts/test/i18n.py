"""Check that every name the app shows has words in every language, and no words are stale."""
import pathlib, re

ROOT = pathlib.Path(__file__).resolve().parents[2]
NAME = r"[A-Z][A-Z0-9_]*"


def without_tests(text):
    """The product code alone. What the tests invent is not shown to anyone."""
    while True:
        start = text.find("#[cfg(test)]")
        if start < 0:
            return text
        opening = text.find("{", start)
        depth, index = 1, opening + 1
        while depth and index < len(text):
            if text[index] == "{":
                depth += 1
            elif text[index] == "}":
                depth -= 1
            index += 1
        text = text[:start] + text[index:]


def call_arguments(text, call):
    """The text inside each call of the given function."""
    for match in re.finditer(r"(?<![A-Za-z_])%s\(" % call, text):
        depth, index = 1, match.end()
        while depth and index < len(text):
            if text[index] == "(":
                depth += 1
            elif text[index] == ")":
                depth -= 1
            index += 1
        yield text[match.end():index - 1]


def names():
    """Every name the app can show, and whether it is given values."""
    found = {}

    def note(name, values):
        found[name] = found.get(name, False) or values

    for path in (ROOT / "src-tauri/src").glob("*.rs"):
        text = without_tests(path.read_text(encoding="utf-8"))
        for call, values in (("message", False), ("message_with", True),
                             ("windows_text", False), ("windows_text_with", True)):
            for name in re.findall(r'(?<![a-z_])%s\(\s*"(%s)"' % (call, NAME), text):
                note(name, values)
    module = (ROOT / "src-native/ksip/ksip.cpp").read_text(encoding="utf-8")
    for assignment in re.findall(r"(?:outcome\s*=|set_outcome\()\s*([^;]*);", module):
        for name in re.findall(r'"(%s)"' % NAME, assignment):
            note(name, False)
    page = (ROOT / "src-web/index.html").read_text(encoding="utf-8")
    for name in re.findall(r'data-i18n(?:-placeholder|-title|-label)?="(%s)"' % NAME, page):
        note(name, False)
    app = (ROOT / "src-web/app.js").read_text(encoding="utf-8")
    for arguments in call_arguments(app, "t"):
        for name in re.findall(r"'(%s)'" % NAME, arguments):
            note(name, False)
    for arguments in call_arguments(app, "fill"):
        chosen = re.findall(r"'(%s)'" % NAME, arguments)
        first = re.match(r"\s*'(%s)'" % NAME, arguments)
        if first:
            note(first.group(1), True)
            chosen = chosen[1:]
        for name in chosen:
            note(name, False)
    return found


def table(text, opening):
    """The names one table answers, and the values each sentence expects."""
    start = text.index(opening) + len(opening)
    depth, index = 1, start
    while depth:
        if text[index] == "{":
            depth += 1
        elif text[index] == "}":
            depth -= 1
        index += 1
    body = text[start:index]
    return {
        name: set(re.findall(r"\{(\d+)\}", sentence))
        for name, sentence in re.findall(r"(%s)\s*:\s*'((?:[^'\\]|\\.)*)'" % NAME, body)
    }


def main():
    reported = names()
    locales = {
        path.stem: table(path.read_text(encoding="utf-8"), "messages:{")
        for path in sorted((ROOT / "src-web/locales").glob("*.js"))
    }
    if "ja" not in locales:
        raise SystemExit("locales/ja.js がありません")
    windows = {
        name: set(re.findall(r"\{(\d+)\}", sentence))
        for name, sentence in re.findall(
            r'"(%s)" => "((?:[^"\\]|\\.)*)"' % NAME,
            (ROOT / "src-tauri/src/message.rs").read_text(encoding="utf-8"),
        )
    }
    japanese = locales["ja"]
    problems = []
    for name, takes_values in sorted(reported.items()):
        holder = japanese if name in japanese else windows if name in windows else None
        if holder is None:
            problems.append("%s: 日本語の文言がありません" % name)
            continue
        if bool(holder[name]) != takes_values:
            problems.append("%s: %s" % (name, "値を渡していますが文言に{0}がありません" if takes_values
                                        else "文言が値を求めていますが渡していません"))
    for name in sorted(set(japanese) - set(reported)):
        problems.append("%s: どこからも使われない文言です" % name)
    for language, words in sorted(locales.items()):
        if language == "ja":
            continue
        for name in sorted(set(japanese) - set(words)):
            problems.append("%s: %s の訳がありません" % (name, language))
        for name in sorted(set(words) - set(japanese)):
            problems.append("%s: %s にだけある文言です" % (name, language))
        for name in sorted(set(words) & set(japanese)):
            if words[name] != japanese[name]:
                problems.append("%s: %s の値の数が日本語と違います" % (name, language))
    if problems:
        print("\n".join(problems))
        raise SystemExit("文言の対応が取れていません: %d件" % len(problems))
    print("names:", len(reported), "| languages:", ", ".join(sorted(locales)),
          "| entries:", len(japanese), "| windows:", len(windows), "：all matched")


if __name__ == "__main__":
    main()
