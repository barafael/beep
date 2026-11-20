"""Molly's gradle/verification-metadata.xml with the checksums of the patched libsignal in.

Molly's metadata lists im.molly:libsignal-client and libsignal-android at one version with the
checksums of the published artifacts; the box builds them itself (testbox/patches/
libsignal-molly-local.patch) and Gradle would refuse them. Each checksum of those two components
becomes that of the file in the Maven repository the libsignal stage published to; every other
component is left as it is, so that Molly's own dependencies are still checked.

Usage: verification.py <version> <metadata in> <repository> <metadata out>
"""

import hashlib
import re
import sys

COMPONENT = re.compile(
    r'(<component group="im\.molly" name="(libsignal-(?:client|android))" version="{version}">)(.*?)(</component>)',
    re.S,
)
ARTIFACT = re.compile(r'(<artifact name="([^"]+)">\s*<sha256 value=")([0-9a-f]+)(")')


def main(version: str, source: str, repository: str, out: str) -> None:
    text = open(source).read()
    replaced = 0

    def component(match: re.Match) -> str:
        name = match.group(2)

        def artifact(found: re.Match) -> str:
            nonlocal replaced
            path = f"{repository}/im/molly/{name}/{version}/{found.group(2)}"
            digest = hashlib.sha256(open(path, "rb").read()).hexdigest()
            replaced += 1
            return f"{found.group(1)}{digest}{found.group(4)}"

        body = ARTIFACT.sub(artifact, match.group(3))
        return f"{match.group(1)}{body}{match.group(4)}"

    pattern = re.compile(COMPONENT.pattern.replace("{version}", re.escape(version)), re.S)
    text = pattern.sub(component, text)
    if replaced < 4:
        sys.exit(f"only {replaced} artifacts of libsignal at {version} found in the metadata")
    open(out, "w").write(text)
    print(f"{replaced} checksums replaced")


if __name__ == "__main__":
    main(*sys.argv[1:5])
