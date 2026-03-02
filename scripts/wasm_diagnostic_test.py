#!/usr/bin/env python3
"""WASM Player diagnostic test: reproduce crossfade, seek hang, and playlist bugs."""

import argparse
import sys
import time
import traceback

from selenium import webdriver
from selenium.webdriver.chrome.options import Options
from selenium.webdriver.chrome.service import Service

PAGE_URL = "http://localhost:8084/"


def make_driver():
    opts = Options()
    opts.set_capability("goog:loggingPrefs", {"browser": "ALL"})
    opts.add_argument("--enable-features=SharedArrayBuffer")
    opts.add_argument("--autoplay-policy=no-user-gesture-required")

    service = Service("/opt/homebrew/bin/chromedriver")
    driver = webdriver.Chrome(service=service, options=opts)
    driver.set_page_load_timeout(30)
    driver.set_script_timeout(15)
    return driver


def drain_logs(driver, log_fn, prefix=""):
    """Drain all browser console logs and write them."""
    count = 0
    try:
        logs = driver.get_log("browser")
        for entry in logs:
            level = entry.get("level", "")
            msg = entry.get("message", "")
            ts = entry.get("timestamp", 0)
            log_fn(f"{ts} [{level}] {prefix}{msg}")
            count += 1
    except Exception:
        pass
    return count


def wait_and_drain(driver, seconds, log_fn, prefix="", interval=0.5):
    """Wait for `seconds`, draining logs every `interval`."""
    total = 0
    iterations = int(seconds / interval)
    for _ in range(iterations):
        time.sleep(interval)
        total += drain_logs(driver, log_fn, prefix)
    return total


def click_playlist_item(driver, pattern):
    """Click playlist item containing `pattern`."""
    driver.execute_script(f"""
        const items = document.querySelectorAll('#playlist li');
        for (const el of items) {{
            if (el.textContent.includes('{pattern}')) {{
                el.click();
                break;
            }}
        }}
    """)


def wait_for_init(driver, log_fn):
    """Wait for WASM init and drain init logs."""
    log_fn("[test] Waiting 6s for WASM init...")
    time.sleep(6)
    drain_logs(driver, log_fn, "INIT: ")


def scenario_a_crossfade(driver, log_fn):
    """Scenario A: Crossfade bug — switch from HLS to MP3."""
    log_fn("\n" + "=" * 80)
    log_fn("[scenario_a] START: Crossfade bug reproduction")
    log_fn("=" * 80)

    # Click HLS track
    log_fn("[scenario_a] Clicking HLS track...")
    click_playlist_item(driver, "HLS")
    time.sleep(1)

    # Click Play
    log_fn("[scenario_a] Clicking Play...")
    driver.execute_script("""
        const buttons = document.querySelectorAll('button');
        for (const btn of buttons) {
            if (btn.textContent.trim().toLowerCase() === 'play') {
                btn.click();
                break;
            }
        }
    """)

    # Wait 5s for playback to start
    log_fn("[scenario_a] Waiting 5s for HLS playback...")
    wait_and_drain(driver, 5, log_fn, "A: ")

    # Click MP3 track
    log_fn("[scenario_a] Clicking MP3 track...")
    click_playlist_item(driver, "MP3")

    # Wait 15s to observe crossfade behavior
    log_fn("[scenario_a] Waiting 15s for crossfade observation...")
    total = wait_and_drain(driver, 15, log_fn, "A: ")

    # Check for diagnostic markers
    log_fn(f"[scenario_a] Collected {total} log entries during crossfade")

    # Screenshot
    try:
        driver.save_screenshot(f"/tmp/wasm_diag_scenario_a.png")
        log_fn("[scenario_a] Screenshot saved")
    except Exception:
        pass

    log_fn("[scenario_a] END")


def check_playback_advancing(driver, log_fn, tag, duration=3.0, interval=0.5):
    """Check that position is advancing (playback not stuck).

    Returns (advancing: bool, last_pos_ms: float).
    """
    positions = []
    steps = int(duration / interval)
    for _ in range(steps):
        time.sleep(interval)
        drain_logs(driver, log_fn, f"{tag}: ")
        try:
            pos = driver.execute_script(
                "return window.__player ? window.__player.get_position_ms() : -1"
            )
            if pos is not None and pos >= 0:
                positions.append(pos)
        except Exception:
            pass

    if len(positions) < 2:
        log_fn(f"[scenario_b] {tag} not enough position samples: {positions}")
        return False, positions[-1] if positions else -1

    advancing = positions[-1] > positions[0] + 100  # at least 100ms progress
    log_fn(
        f"[scenario_b] {tag} positions: first={positions[0]:.0f} "
        f"last={positions[-1]:.0f} advancing={advancing}"
    )
    return advancing, positions[-1]


def seek_and_verify(driver, log_fn, target_ms, label, timeout_s=10):
    """Seek to target_ms, wait for position to reach the range, then check playback advances.

    Returns (seek_ok, playback_ok, final_pos).
    """
    log_fn(f"[scenario_b] Seeking to {label}...")
    driver.execute_script(f"window.__player && window.__player.seek({target_ms})")

    # Wait for seek to land near target
    low = max(0, target_ms - 5000)
    high = target_ms + 15000
    seek_ok = False
    pos = -1
    for _ in range(int(timeout_s / 0.5)):
        time.sleep(0.5)
        drain_logs(driver, log_fn, "B: ")
        try:
            pos = driver.execute_script(
                "return window.__player ? window.__player.get_position_ms() : -1"
            )
            if pos is not None and low <= pos <= high:
                seek_ok = True
                break
        except Exception:
            pass

    if not seek_ok:
        log_fn(f"[scenario_b] WARNING: seek to {label} may have hung (pos={pos})")
        return False, False, pos

    log_fn(f"[scenario_b] Seek to {label} landed at {pos:.0f}ms")

    # Check that playback keeps advancing after seek
    advancing, final_pos = check_playback_advancing(
        driver, log_fn, f"after-{label}", duration=3.0
    )
    if not advancing:
        log_fn(f"[scenario_b] WARNING: playback STUCK after seek to {label}")
    return seek_ok, advancing, final_pos


def scenario_b_seek_hang(driver, log_fn):
    """Scenario B: Backward seek hang on MP3 — two full forward+backward cycles."""
    log_fn("\n" + "=" * 80)
    log_fn("[scenario_b] START: Backward seek hang reproduction (2 cycles)")
    log_fn("=" * 80)

    # Click MP3 track
    log_fn("[scenario_b] Clicking MP3 track...")
    click_playlist_item(driver, "MP3")
    time.sleep(1)

    # Click Play
    log_fn("[scenario_b] Clicking Play...")
    driver.execute_script("""
        const buttons = document.querySelectorAll('button');
        for (const btn of buttons) {
            if (btn.textContent.trim().toLowerCase() === 'play') {
                btn.click();
                break;
            }
        }
    """)

    log_fn("[scenario_b] Waiting 3s for playback start...")
    adv, _ = check_playback_advancing(driver, log_fn, "initial", duration=3.0)
    if not adv:
        log_fn("[scenario_b] WARNING: playback did not start")

    results = []

    # --- Cycle 1: forward 50s -> 100s -> backward 12s ---
    log_fn("[scenario_b] --- Cycle 1 ---")
    s1_ok, p1_ok, _ = seek_and_verify(driver, log_fn, 50000, "50s")
    s2_ok, p2_ok, _ = seek_and_verify(driver, log_fn, 100000, "100s")
    s3_ok, p3_ok, _ = seek_and_verify(driver, log_fn, 12000, "12s (backward)")
    results.append(("cycle1-fwd50", s1_ok, p1_ok))
    results.append(("cycle1-fwd100", s2_ok, p2_ok))
    results.append(("cycle1-bwd12", s3_ok, p3_ok))

    # --- Cycle 2: forward 80s -> backward 5s ---
    log_fn("[scenario_b] --- Cycle 2 ---")
    s4_ok, p4_ok, _ = seek_and_verify(driver, log_fn, 80000, "80s")
    s5_ok, p5_ok, _ = seek_and_verify(driver, log_fn, 5000, "5s (backward)")
    results.append(("cycle2-fwd80", s4_ok, p4_ok))
    results.append(("cycle2-bwd5", s5_ok, p5_ok))

    # Summary
    log_fn("[scenario_b] --- Results ---")
    all_ok = True
    for label, sok, pok in results:
        status = "OK" if (sok and pok) else "FAIL"
        if not sok:
            status += " (seek hung)"
        elif not pok:
            status += " (playback stuck)"
        log_fn(f"[scenario_b]   {label}: {status}")
        if not (sok and pok):
            all_ok = False

    if all_ok:
        log_fn("[scenario_b] All seeks and playback checks PASSED")
    else:
        log_fn("[scenario_b] WARNING: Some seeks or playback checks FAILED")

    # Screenshot
    try:
        driver.save_screenshot("/tmp/wasm_diag_scenario_b.png")
        log_fn("[scenario_b] Screenshot saved")
    except Exception:
        pass

    log_fn("[scenario_b] END")


def scenario_c_playlist_crash(driver, log_fn):
    """Scenario C: Rapid track switching — playlist crash."""
    log_fn("\n" + "=" * 80)
    log_fn("[scenario_c] START: Playlist crash reproduction")
    log_fn("=" * 80)

    # Rapid-fire click 3 different tracks
    log_fn("[scenario_c] Rapid track switching (3 times)...")
    for i, pattern in enumerate(["HLS", "MP3", "HLS"]):
        log_fn(f"[scenario_c] Click #{i+1}: {pattern}")
        click_playlist_item(driver, pattern)
        time.sleep(0.3)

    # Wait for things to settle
    wait_and_drain(driver, 5, log_fn, "C: ")

    # Check playlist DOM
    try:
        children = driver.execute_script(
            "return document.querySelector('#playlist') "
            "? document.querySelector('#playlist').children.length : -1"
        )
        log_fn(f"[scenario_c] Playlist children count: {children}")
        if children == 0:
            log_fn("[scenario_c] WARNING: Playlist is empty — possible crash")
        elif children == -1:
            log_fn("[scenario_c] WARNING: #playlist element not found")
    except Exception as e:
        log_fn(f"[scenario_c] Error checking playlist: {e}")

    # Screenshot
    try:
        driver.save_screenshot(f"/tmp/wasm_diag_scenario_c.png")
        log_fn("[scenario_c] Screenshot saved")
    except Exception:
        pass

    log_fn("[scenario_c] END")


def scenario_d_hls_playback(driver, log_fn):
    """Scenario D: HLS playback and seek — verify no regression."""
    log_fn("\n" + "=" * 80)
    log_fn("[scenario_d] START: HLS playback and seek")
    log_fn("=" * 80)

    # Click HLS track
    log_fn("[scenario_d] Clicking HLS track...")
    click_playlist_item(driver, "HLS")
    time.sleep(1)

    # Click Play
    log_fn("[scenario_d] Clicking Play...")
    driver.execute_script("""
        const buttons = document.querySelectorAll('button');
        for (const btn of buttons) {
            if (btn.textContent.trim().toLowerCase() === 'play') {
                btn.click();
                break;
            }
        }
    """)

    # Check initial playback advancing
    log_fn("[scenario_d] Checking initial HLS playback (5s)...")
    adv, pos = check_playback_advancing(driver, log_fn, "hls-initial", duration=5.0)
    if not adv:
        log_fn("[scenario_d] WARNING: HLS playback did not start")

    # Seek forward to 30s
    s1_ok, p1_ok, _ = seek_and_verify(driver, log_fn, 30000, "hls-30s")
    # Seek forward to 60s
    s2_ok, p2_ok, _ = seek_and_verify(driver, log_fn, 60000, "hls-60s")
    # Seek backward to 10s
    s3_ok, p3_ok, _ = seek_and_verify(driver, log_fn, 10000, "hls-10s (backward)")

    results = [
        ("hls-initial", True, adv),
        ("hls-fwd30", s1_ok, p1_ok),
        ("hls-fwd60", s2_ok, p2_ok),
        ("hls-bwd10", s3_ok, p3_ok),
    ]

    log_fn("[scenario_d] --- Results ---")
    all_ok = True
    for label, sok, pok in results:
        status = "OK" if (sok and pok) else "FAIL"
        if not sok:
            status += " (seek hung)"
        elif not pok:
            status += " (playback stuck)"
        log_fn(f"[scenario_d]   {label}: {status}")
        if not (sok and pok):
            all_ok = False

    if all_ok:
        log_fn("[scenario_d] All HLS checks PASSED")
    else:
        log_fn("[scenario_d] WARNING: Some HLS checks FAILED")

    log_fn("[scenario_d] END")


def run_diagnostics(run_number):
    log_file = f"/tmp/wasm_diagnostic_run_{run_number}.txt"
    out = open(log_file, "w")

    def log_fn(msg):
        out.write(msg + "\n")
        out.flush()
        print(msg, flush=True)

    log_fn(f"[test] Diagnostic run #{run_number}")
    log_fn(f"[test] Log file: {log_file}")

    driver = None
    try:
        driver = make_driver()
        log_fn(f"[test] Opening {PAGE_URL}")
        driver.get(PAGE_URL)
        wait_for_init(driver, log_fn)

        # Run all three scenarios
        scenario_a_crossfade(driver, log_fn)

        # Reload page between scenarios for clean state
        log_fn("\n[test] Reloading page for scenario B...")
        driver.get(PAGE_URL)
        wait_for_init(driver, log_fn)
        scenario_b_seek_hang(driver, log_fn)

        # Reload page between scenarios for clean state
        log_fn("\n[test] Reloading page for scenario C...")
        driver.get(PAGE_URL)
        wait_for_init(driver, log_fn)
        scenario_c_playlist_crash(driver, log_fn)

        # Reload page between scenarios for clean state
        log_fn("\n[test] Reloading page for scenario D...")
        driver.get(PAGE_URL)
        wait_for_init(driver, log_fn)
        scenario_d_hls_playback(driver, log_fn)

        log_fn("\n[test] All scenarios completed")

    except Exception as e:
        log_fn(f"[test] EXCEPTION: {e}")
        traceback.print_exc(file=out)

        if driver:
            try:
                driver.save_screenshot(f"/tmp/wasm_diag_run_{run_number}_error.png")
            except Exception:
                pass
    finally:
        if driver:
            try:
                driver.quit()
            except Exception:
                pass
        out.close()

    print(f"[test] Run #{run_number} complete. Logs: {log_file}")


def main():
    parser = argparse.ArgumentParser(description="WASM Player diagnostic test")
    parser.add_argument("--run", type=int, default=1, help="Run number (for log file naming)")
    args = parser.parse_args()

    run_diagnostics(args.run)


if __name__ == "__main__":
    main()
