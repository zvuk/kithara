#!/usr/bin/env python3
"""Open WASM player in Chrome, start HLS playback, collect console logs incrementally."""

import sys
import time
import traceback

from selenium import webdriver
from selenium.webdriver.chrome.options import Options
from selenium.webdriver.chrome.service import Service

HLS_URL = "https://stream.silvercomet.top/hls/master.m3u8"
PAGE_URL = "http://localhost:8084/"
LOG_FILE = "/tmp/wasm_hls_log.txt"


def main():
    out = open(LOG_FILE, "w")

    def log(msg):
        out.write(msg + "\n")
        out.flush()
        print(msg, flush=True)

    opts = Options()
    opts.set_capability("goog:loggingPrefs", {"browser": "ALL"})
    opts.add_argument("--enable-features=SharedArrayBuffer")
    opts.add_argument("--autoplay-policy=no-user-gesture-required")

    service = Service("/opt/homebrew/bin/chromedriver")
    log("[test] Starting Chrome...")
    driver = webdriver.Chrome(service=service, options=opts)
    driver.set_page_load_timeout(30)
    driver.set_script_timeout(10)

    try:
        log(f"[test] Opening {PAGE_URL}")
        driver.get(PAGE_URL)
        log("[test] Page loaded, waiting 5s for WASM init...")
        time.sleep(5)

        # Drain any init logs
        try:
            init_logs = driver.get_log("browser")
            log(f"[test] Init logs: {len(init_logs)} entries")
            for entry in init_logs:
                log(f"  INIT: {entry.get('message', '')[:200]}")
        except:
            pass

        # Click HLS playlist item
        log("[test] Clicking HLS playlist item...")
        driver.execute_script("""
            const items = document.querySelectorAll('*');
            for (const el of items) {
                if (el.childElementCount === 0 && el.textContent.includes('master.m3u8')) {
                    el.click();
                    break;
                }
            }
        """)
        time.sleep(0.5)

        # Click Play
        log("[test] Clicking Play...")
        driver.execute_script("""
            const buttons = document.querySelectorAll('button');
            for (const btn of buttons) {
                if (btn.textContent.trim().toLowerCase() === 'play') {
                    btn.click();
                    break;
                }
            }
        """)

        # Collect logs in a loop every 0.5s for 20 seconds
        log("[test] Collecting logs in loop...")
        log("=" * 120)
        total_collected = 0
        for i in range(40):  # 40 * 0.5 = 20 seconds
            time.sleep(0.5)
            try:
                logs = driver.get_log("browser")
                if logs:
                    for entry in logs:
                        level = entry.get("level", "")
                        msg = entry.get("message", "")
                        ts = entry.get("timestamp", 0)
                        log(f"{ts} [{level}] {msg}")
                    total_collected += len(logs)
            except Exception as e:
                log(f"[test] Log poll failed at iteration {i}: {e}")
                break

        log("=" * 120)
        log(f"[test] Total collected: {total_collected}")

        # Screenshot
        try:
            driver.save_screenshot("/tmp/wasm_page_after.png")
            log("[test] Screenshot saved")
        except:
            log("[test] Screenshot failed")

    except Exception as e:
        log(f"[test] EXCEPTION: {e}")
        traceback.print_exc(file=out)
    finally:
        try:
            driver.quit()
        except:
            pass
        out.close()


if __name__ == "__main__":
    main()
