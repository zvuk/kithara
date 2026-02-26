#!/usr/bin/env python3
"""Selenium scenarios for WASM player track switching, seek, and replay behavior."""

from __future__ import annotations

import argparse
import sys
import time
import traceback
from pathlib import Path
from typing import Any, Callable

from selenium import webdriver
from selenium.webdriver.chrome.options import Options
from selenium.webdriver.chrome.service import Service
from selenium.webdriver.common.by import By
from selenium.common.exceptions import JavascriptException, WebDriverException


class ScenarioError(RuntimeError):
    """Raised when an expected player behavior does not happen in time."""


class WasmPlayerScenario:
    def __init__(
        self,
        *,
        page_url: str,
        chromedriver_path: str,
        headless: bool,
        switch_wait_seconds: int,
        timeout_seconds: int,
    ) -> None:
        self.page_url = page_url
        self.switch_wait_seconds = switch_wait_seconds
        self.timeout_seconds = timeout_seconds

        opts = Options()
        opts.set_capability("goog:loggingPrefs", {"browser": "ALL"})
        opts.add_argument("--enable-features=SharedArrayBuffer")
        opts.add_argument("--autoplay-policy=no-user-gesture-required")
        if headless:
            opts.add_argument("--headless=new")

        service = Service(chromedriver_path)
        self.driver = webdriver.Chrome(service=service, options=opts)
        self.driver.set_page_load_timeout(60)
        self.driver.set_script_timeout(30)

    def close(self) -> None:
        self.driver.quit()

    def log(self, message: str) -> None:
        print(message, flush=True)

    def snapshot(self) -> dict[str, Any]:
        for _ in range(20):
            try:
                return self.driver.execute_script(
                    """
                    const slider = document.getElementById('seek-slider');
                    const status = document.getElementById('status')?.textContent ?? '';
                    const playlist = [...document.querySelectorAll('#playlist li')].map((el) => el.textContent ?? '');
                    const out = {
                        status,
                        playlist,
                        seek_value: slider ? Number(slider.value) : null,
                        seek_max: slider ? Number(slider.max) : null,
                    };
                    if (window.__player) {
                        out.idx = window.__player.current_index();
                        out.len = window.__player.playlist_len();
                        out.pc = window.__player.process_count();
                        out.pos = window.__player.get_position_ms();
                        out.dur = window.__player.get_duration_ms();
                    }
                    return out;
                    """
                )
            except (JavascriptException, WebDriverException):
                time.sleep(0.25)
        return {
            "status": "<snapshot-unavailable>",
            "playlist": [],
            "seek_value": None,
            "seek_max": None,
        }

    def browser_logs(self) -> list[dict[str, Any]]:
        try:
            return self.driver.get_log("browser")
        except Exception:
            return []

    def dump_debug(self, prefix: str) -> None:
        try:
            snap = self.snapshot()
            self.log(f"[{prefix}] snapshot={snap}")
        except Exception as exc:  # noqa: BLE001
            self.log(f"[{prefix}] snapshot unavailable: {exc}")
        for entry in self.browser_logs()[-120:]:
            level = entry.get("level")
            msg = entry.get("message", "")
            if any(
                token in msg.lower()
                for token in (
                    "error",
                    "failed",
                    "wait_range",
                    "[worker]",
                    "[player]",
                    "[diag]",
                )
            ):
                self.log(f"[{prefix}] browser {level}: {msg}")

    def wait_for(
        self,
        description: str,
        condition: Callable[[dict[str, Any]], bool],
        *,
        timeout: float | None = None,
        interval: float = 0.25,
    ) -> dict[str, Any]:
        deadline = time.time() + (timeout if timeout is not None else self.timeout_seconds)
        last: dict[str, Any] = {}
        while time.time() < deadline:
            try:
                last = self.snapshot()
            except Exception:  # noqa: BLE001
                time.sleep(interval)
                continue
            if condition(last):
                return last
            time.sleep(interval)
        raise ScenarioError(f"Timeout waiting for: {description}; last_snapshot={last}")

    def open(self) -> None:
        self.log(f"[scenario] Opening {self.page_url}")
        self.driver.get(self.page_url)
        self.wait_for(
            "player bootstrap",
            lambda s: (s.get("len") or 0) >= 2 and len(s.get("playlist", [])) >= 2,
            timeout=45,
        )
        self.log(f"[scenario] Bootstrapped: {self.snapshot()}")

    def click_playlist_item(self, index: int) -> None:
        items = self.driver.find_elements(By.CSS_SELECTOR, "#playlist li")
        if len(items) <= index:
            raise ScenarioError(f"Playlist item {index} not found; count={len(items)}")
        items[index].click()

    def click_button(self, button_id: str) -> None:
        self.driver.find_element(By.ID, button_id).click()

    def select_track_and_wait(self, index: int, label: str) -> None:
        self.log(f"[scenario] Selecting {label} (index={index})")
        self.click_playlist_item(index)

        self.wait_for(
            f"track {label} selected",
            lambda s: s.get("idx") == index,
            timeout=45,
        )
        # Explicit Play is required for stable Selenium runs.
        self.click_button("play-btn")
        self.wait_for(
            f"track {label} duration known",
            lambda s: (s.get("dur") or 0) > 0,
            timeout=45,
        )
        self.assert_motion(f"{label} playback started", window=4.0, min_pos_delta_ms=350)

        snap = self.snapshot()
        if len(snap.get("playlist", [])) < 2:
            raise ScenarioError(f"Playlist collapsed after selecting {label}: {snap}")

        self.log(f"[scenario] {label} selected: {snap}")

    def assert_motion(self, description: str, *, window: float, min_pos_delta_ms: float) -> None:
        start = self.snapshot()
        time.sleep(window)
        end = self.snapshot()
        pos0 = float(start.get("pos") or 0)
        pos1 = float(end.get("pos") or 0)
        pc0 = float(start.get("pc") or 0)
        pc1 = float(end.get("pc") or 0)

        if pos1 - pos0 >= min_pos_delta_ms:
            return
        if pc1 - pc0 >= 5:
            return

        raise ScenarioError(
            f"{description}: playback did not advance enough; "
            f"start={start} end={end}"
        )

    def wait_between_switches(self, label: str) -> None:
        self.log(f"[scenario] Waiting {self.switch_wait_seconds}s before next switch ({label})")
        self.assert_motion(
            f"{label} playback stable before wait",
            window=2.0,
            min_pos_delta_ms=300,
        )
        time.sleep(self.switch_wait_seconds)
        self.assert_motion(
            f"{label} playback stable after wait",
            window=2.0,
            min_pos_delta_ms=300,
        )

    def seek_ratio(self, ratio: float, *, tolerance_ms: float) -> float:
        snap = self.snapshot()
        dur = float(snap.get("dur") or 0)
        if dur <= 0:
            raise ScenarioError(f"Cannot seek: duration is unknown; snapshot={snap}")

        ratio = max(0.0, min(1.0, ratio))
        target = dur * ratio

        self.driver.execute_script(
            """
            const slider = document.getElementById('seek-slider');
            const target = arguments[0];
            slider.value = String(target);
            slider.dispatchEvent(new Event('input', { bubbles: true }));
            slider.dispatchEvent(new Event('change', { bubbles: true }));
            """,
            target,
        )

        def close_enough(s: dict[str, Any]) -> bool:
            pos = float(s.get("pos") or 0)
            return abs(pos - target) <= tolerance_ms

        self.wait_for(
            f"seek to ratio {ratio:.3f} (target={target:.0f}ms)",
            close_enough,
            timeout=30,
            interval=0.2,
        )
        self.log(f"[scenario] Seek ratio={ratio:.3f} target={target:.0f}ms done")
        return target

    def set_volume_percent(self, percent: int) -> None:
        if percent < 0 or percent > 100:
            raise ScenarioError(f"volume percent out of range: {percent}")
        value = percent / 100.0
        self.driver.execute_script(
            """
            const slider = document.getElementById('volume-slider');
            slider.value = String(arguments[0]);
            slider.dispatchEvent(new Event('input', { bubbles: true }));
            slider.dispatchEvent(new Event('change', { bubbles: true }));
            """,
            value,
        )
        self.wait_for(
            f"volume set to {percent}%",
            lambda s: abs(float(self.driver.execute_script("return window.__player.get_volume();")) - value)
            < 0.011,
            timeout=10,
        )
        self.log(f"[scenario] Volume set to {percent}%")

    def seek_to_start_and_verify(self, *, require_play_click: bool) -> None:
        self.seek_ratio(0.0, tolerance_ms=2000)
        if require_play_click:
            self.click_button("play-btn")
        self.wait_for(
            "position near start after seek",
            lambda s: float(s.get("pos") or 0) <= 2500,
            timeout=10,
        )
        self.assert_motion("playback resumes from start", window=3.0, min_pos_delta_ms=250)

    def stop_and_replay_verify(self) -> None:
        self.click_button("stop-btn")
        self.wait_for(
            "stop state reflected in UI",
            lambda s: "Stopped" in str(s.get("status", "")) and float(s.get("pos") or 0) <= 2500,
            timeout=10,
        )
        self.click_button("play-btn")
        self.wait_for(
            "play after stop starts from beginning",
            lambda s: float(s.get("pos") or 0) <= 4000,
            timeout=10,
        )
        self.assert_motion("stop->play restart motion", window=3.0, min_pos_delta_ms=250)

    def run_track_scenario(self, index: int, label: str) -> None:
        self.select_track_and_wait(index, label)

        # First step requested: set volume to 25%.
        self.set_volume_percent(25)

        self.wait_between_switches(label)

        self.seek_ratio(0.30, tolerance_ms=7000)
        self.assert_motion(f"{label} motion after 30% seek", window=3.0, min_pos_delta_ms=250)

        self.seek_ratio(0.60, tolerance_ms=9000)
        self.assert_motion(f"{label} motion after 60% seek", window=3.0, min_pos_delta_ms=250)

        self.seek_ratio(0.95, tolerance_ms=15000)
        self.assert_motion(f"{label} motion after near-end seek", window=2.5, min_pos_delta_ms=150)

        self.seek_to_start_and_verify(require_play_click=False)

        self.seek_ratio(0.95, tolerance_ms=15000)
        self.seek_to_start_and_verify(require_play_click=True)

        self.seek_ratio(0.95, tolerance_ms=15000)
        self.stop_and_replay_verify()

    def run(self) -> None:
        self.open()

        self.run_track_scenario(0, "MP3")
        self.run_track_scenario(1, "HLS")

        # Additional switches every 10-15s.
        self.select_track_and_wait(0, "MP3")
        self.wait_between_switches("MP3")

        self.select_track_and_wait(1, "HLS")
        self.wait_between_switches("HLS")

        self.log("[scenario] All scenarios passed")


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--url",
        default="http://127.0.0.1:9092/",
        help="WASM demo URL",
    )
    parser.add_argument(
        "--chromedriver",
        default="/opt/homebrew/bin/chromedriver",
        help="Path to chromedriver",
    )
    parser.add_argument(
        "--switch-wait-seconds",
        type=int,
        default=12,
        help="Seconds to wait between track switches (should stay in 10-15 range)",
    )
    parser.add_argument(
        "--timeout-seconds",
        type=int,
        default=45,
        help="Default wait timeout for async UI/player transitions",
    )
    parser.add_argument(
        "--headless",
        action="store_true",
        help="Run Chrome in headless mode",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    scenario = WasmPlayerScenario(
        page_url=args.url,
        chromedriver_path=args.chromedriver,
        headless=args.headless,
        switch_wait_seconds=args.switch_wait_seconds,
        timeout_seconds=args.timeout_seconds,
    )

    try:
        scenario.run()
        return 0
    except Exception as exc:  # noqa: BLE001
        print(f"[scenario] FAILED: {exc}", file=sys.stderr, flush=True)
        traceback.print_exc()
        try:
            screenshot_path = Path("/tmp/wasm_player_scenarios_failed.png")
            scenario.driver.save_screenshot(str(screenshot_path))
            print(f"[scenario] screenshot: {screenshot_path}", file=sys.stderr, flush=True)
        except Exception:
            pass
        scenario.dump_debug("failure")
        return 1
    finally:
        scenario.close()


if __name__ == "__main__":
    raise SystemExit(main())
