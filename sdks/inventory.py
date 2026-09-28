"""Extract registered routes and wire annotations; fail on unknown Rust types."""

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def routes():
    result = []
    filenames = ("api.rs", "profiles.rs", "files.rs", "browser.rs", "remote.rs", "sharing.rs")
    for source in (ROOT / "crates/haarness-server/src").glob("*.rs"):
        if re.search(r'\.route\(\s*"', source.read_text().split("#[cfg(test)]")[0]):
            assert source.name in filenames, f"New route module needs SDK coverage: {source.name}"
    for filename in filenames:
        text = (ROOT / "crates/haarness-server/src" / filename).read_text()
        for m in re.finditer(r'\.route\(\s*"([^"]+)",', text):
            pos = m.end()
            depth = 1
            end = pos
            while depth:
                depth += (text[end] == "(") - (text[end] == ")")
                end += 1
            for method, handler in re.findall(
                r"\b(get|post|put|patch|delete)\((\w+)\)", text[pos:end]
            ):
                path = m[1] if m[1].startswith("/v1/") or m[1] in ("/healthz", "/openapi.json") or filename == "sharing.rs" else "/v1" + m[1]
                name = (
                    {
                        "list": "list_profiles",
                        "upload": "upload_profile",
                        "metadata": "get_profile",
                        "download": "download_profile",
                        "remove": "delete_profile",
                    }.get(handler, handler)
                    if filename == "profiles.rs"
                    else handler
                )
                if filename == "remote.rs":
                    name = {"catalog":"host_projects","browse":"host_directories","open":"host_open","close_project":"host_close"}[handler]
                if filename == "browser.rs":
                    name = {"attach": "connect_browser_host", "attach_draft": "connect_draft_browser_host", "tunnel": "browser_tunnel", "draft_tunnel": "draft_browser_tunnel", "adopt_draft": "adopt_browser_draft"}.get(handler, handler)
                if filename == "sharing.rs": name = "read_shared_session"
                fn = re.search(r"async fn " + handler + r"\b([\s\S]*?)\{", text[text.index("fn failure"): ] if filename == "remote.rs" else text)
                result.append((name, method.upper(), path, fn[1], filename))
    return result


if __name__ == "__main__":
    for name, method, path, sig, file in routes():
        print(name, method, path, re.sub(r"\s+", " ", sig))
