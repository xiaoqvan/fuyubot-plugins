#!/usr/bin/env python3
"""汽水音乐扫码登录：生成二维码 -> 轮询 -> 打印可以贴进 fuyubot.toml 的 soda cookie。

只用 Python 标准库，不需要安装依赖。

用法：
    python scripts/qishui_qr_login.py
    python scripts/qishui_qr_login.py --no-open --timeout 180
    python scripts/qishui_qr_login.py --output C:/cache/qishui_qr.png

说明：
- 汽水网页版不能登录，所以只提供扫码这一种方式。
- 二维码来自汽水官方 passport 接口，只用于本次登录；扫码要用**已登录的抖音 APP**，
  汽水 App 扫码通常不会完成确认（接口文案明确要求抖音）。
- 成功后脚本只打印 cookie，不会自动改配置文件；把它贴进
  [plugin.fuyubot.music] 的 soda = "..."，重启宿主即可。
- 扫码登录只拿登录态（sessionid 等）；会员无损音质还需要 soda_x_helios /
  soda_x_medusa（本机签名器），这不在扫码范围内。
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import random
import subprocess
import sys
import time
import urllib.parse
import urllib.request
from pathlib import Path

BASE = "https://api.qishui.com"
UA = (
    "Mozilla/5.0 (Windows NT 10.0; Win64; x64) "
    "AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120 Safari/537.36"
)
QR_URL = f"{BASE}/passport/web/get_qrcode/"
CHECK_URL = f"{BASE}/passport/web/check_qrconnect/"
AID = "386088"
FIXED = {
    "passport_jssdk_version": "2.4.13",
    "passport_jssdk_type": "normal",
    "is_from_ttaccountsdk": "1",
    "next": "https://api.qishui.com",
    "need_logo": "false",
    "need_short_url": "false",
    "is_new_login": "1",
}


def _iid() -> str:
    env = os.environ.get("QISHUI_IID", "").strip()
    if env:
        return env
    return str(random.randint(10**15, 10**16 - 1))


def _request(url: str, *, data: bytes | None = None, headers: dict[str, str] | None = None):
    merged = {"User-Agent": UA}
    if headers:
        merged.update(headers)
    return urllib.request.urlopen(
        urllib.request.Request(url, data=data, headers=merged), timeout=30
    )


def _cookies(response) -> list[str]:
    """从 Set-Cookie 里取 name=value（去掉属性、过期项），同名保留最后一个。"""
    raw_values = response.headers.get_all("Set-Cookie") or []
    ordered: list[tuple[str, str]] = []
    for raw in raw_values:
        first = raw.split(";", 1)[0].strip()
        if "=" not in first:
            continue
        name, value = first.split("=", 1)
        name, value = name.strip(), value.strip()
        if not name or not value:
            continue
        ordered = [(n, v) for (n, v) in ordered if n != name]
        ordered.append((name, value))
    return [f"{name}={value}" for name, value in ordered]


def _cookie_header(cookies: list[str], only: tuple[str, ...] = ()) -> str:
    # 同名只保留最后一个，避免 Set-Cookie 与 body 里的 sessionid 重复。
    seen: dict[str, str] = {}
    for cookie in cookies:
        name, _, value = cookie.partition("=")
        if not name or not value:
            continue
        if only and name not in only:
            continue
        seen[name] = value
    return "; ".join(f"{name}={value}" for name, value in seen.items())


def fetch_qrcode() -> tuple[dict, list[str]]:
    query = dict(FIXED, aid=AID)
    url = QR_URL + "?" + urllib.parse.urlencode(query)
    with _request(url) as response:
        body = json.loads(response.read())
        cookies = _cookies(response)
    data = body.get("data") if isinstance(body.get("data"), dict) else body
    if not data.get("token") or not data.get("qrcode"):
        raise RuntimeError(f"二维码接口返回异常：{json.dumps(body, ensure_ascii=False)[:300]}")
    return data, cookies


def save_qr(data_uri: str, output: Path) -> Path:
    if data_uri.startswith("data:image"):
        payload = data_uri.split(",", 1)[1]
        output.write_bytes(base64.b64decode(payload))
    elif data_uri.startswith("http"):
        with _request(data_uri) as response:
            output.write_bytes(response.read())
    else:
        raise RuntimeError("二维码既不是 data URI 也不是图片 URL")
    return output


def open_image(path: Path) -> None:
    try:
        if os.name == "nt":
            os.startfile(str(path))  # type: ignore[attr-defined]
        elif sys.platform == "darwin":
            subprocess.run(["open", str(path)], check=False)
        else:
            subprocess.run(["xdg-open", str(path)], check=False)
    except Exception:
        pass


def check_status(token: str, iid: str, cookies: list[str]) -> tuple[str, list[str], dict]:
    query = {
        "passport_jssdk_version": FIXED["passport_jssdk_version"],
        "passport_jssdk_type": FIXED["passport_jssdk_type"],
        "is_from_ttaccountsdk": FIXED["is_from_ttaccountsdk"],
        "aid": AID,
        "iid": iid,
    }
    body = urllib.parse.urlencode(
        {
            "need_logo": FIXED["need_logo"],
            "need_short_url": FIXED["need_short_url"],
            "is_frontier": "true",
            "token": token,
            "is_new_login": FIXED["is_new_login"],
            "next": FIXED["next"],
        }
    ).encode()
    headers = {"Content-Type": "application/x-www-form-urlencoded"}
    cookie_header = _cookie_header(cookies)
    if cookie_header:
        headers["Cookie"] = cookie_header
    with _request(CHECK_URL + "?" + urllib.parse.urlencode(query), data=body, headers=headers) as response:
        payload = json.loads(response.read())
        fresh = _cookies(response)
    data = payload.get("data") if isinstance(payload.get("data"), dict) else payload
    return str(data.get("status", "")), fresh, payload


def _cookies_from_payload(fresh: list[str], payload: dict) -> list[str]:
    merged = list(fresh)
    data = payload.get("data") if isinstance(payload.get("data"), dict) else {}
    direct = str(data.get("sessionid", "")).strip()
    if direct:
        merged.append(f"sessionid={direct}")
    redirect = str(data.get("redirect_url", ""))
    if redirect and "?" in redirect:
        for pair in redirect.split("?", 1)[1].split("&"):
            key, _, value = pair.partition("=")
            if key == "sessionid" and value:
                merged.append("sessionid=" + urllib.parse.unquote(value))
    return merged


def main() -> int:
    parser = argparse.ArgumentParser(description="汽水音乐扫码登录（输出 soda cookie）")
    parser.add_argument("--output", default="qishui_qr.png", help="二维码 PNG 保存路径")
    parser.add_argument("--no-open", action="store_true", help="不自动打开二维码图片")
    parser.add_argument("--timeout", type=int, default=180, help="等待扫码的秒数（默认 180）")
    parser.add_argument("--interval", type=float, default=2.0, help="轮询间隔秒数（默认 2）")
    args = parser.parse_args()

    data, cookies = fetch_qrcode()
    token = str(data["token"])
    iid = _iid()
    output = Path(args.output).expanduser().resolve()
    output.parent.mkdir(parents=True, exist_ok=True)
    save_qr(str(data["qrcode"]), output)
    print(f"二维码已保存：{output}")
    if data.get("qrcode_index_url"):
        print(f"二维码链接：{data['qrcode_index_url']}")
    print("请用**已登录的抖音 APP**扫码；汽水 App 扫码通常不会完成确认。")
    if not args.no_open:
        open_image(output)

    deadline = time.time() + max(10, args.timeout)
    last_status = ""
    while time.time() < deadline:
        time.sleep(max(0.5, args.interval))
        try:
            status, fresh, payload = check_status(token, iid, cookies)
        except Exception as error:
            print(f"轮询失败（会重试）：{error}")
            continue
        if status == "scanned" and last_status != status:
            print("已扫码，请在手机上点「确认登录」……")
        elif status == "confirmed":
            merged = _cookies_from_payload(fresh, payload)
            cookie = _cookie_header(merged)
            if not cookie:
                print("上游确认登录了，但响应里没有 sessionid；请重试。")
                return 2
            print("\n登录成功。把下面这一行贴进 fuyubot.toml 的 [plugin.fuyubot.music]：\n")
            print(f'soda = "{cookie}"')
            print("\n然后重启宿主。会员无损还需要 soda_x_helios / soda_x_medusa 签名头。")
            return 0
        elif status in {"expired", "canceled", "cancelled"}:
            print(f"二维码已失效（status={status}），请重新运行。")
            return 2
        elif status and status != last_status:
            print(f"等待扫码（status={status}）……")
        last_status = status

    print("等待超时，请重新运行脚本。")
    return 2


if __name__ == "__main__":
    raise SystemExit(main())
