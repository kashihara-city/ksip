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
restore("re", re_base, ["cmake/re-config.cmake"])
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

baresip = ROOT / "temp/vendor/baresip"
restore("baresip", baresip, ["src/main.c", "CMakeLists.txt"])
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

shutil.copytree(ROOT / "src-native/ksip_audio",
                baresip / "modules/ksip_audio", dirs_exist_ok=True)
shutil.copytree(ROOT / "src-native/postlab",
                baresip / "modules/postlab", dirs_exist_ok=True)
shutil.copytree(ROOT / "src-native/ksip",
                baresip / "modules/ksip", dirs_exist_ok=True)

cmake = baresip / "CMakeLists.txt"
cmake.write_text(cmake.read_text() + "\n# KSIP embedded entry point\n" +
                 (ROOT / "src-native/embedded.cmake").read_text())
