"""Linux system-tray glue.

Wraps ``pystray`` with the same state-driven icon scheme as the Rust
client: one 64x64 RGBA image per state (idle/recording/uploading/error)
generated procedurally with Pillow, no binary assets in-tree.

User-visible feedback policy:
* Icon colour is the primary state indicator.
* The tooltip (``icon.title``) is a one-liner kept in sync with state.
* Parse errors and reload results log to stderr. ``pystray`` exposes
  ``notify()`` which tries libnotify first and falls back to stdout; we
  use it for successful hot-reloads and parse-error surfacing.

The module is import-safe without ``pystray`` installed so tests can
cover the enums, icon generation, and menu wiring without pulling in
the X11/GTK dependency chain. ``run_tray()`` raises ``RuntimeError`` if
``pystray`` is missing at call time.
"""

from __future__ import annotations

import enum
import sys
import threading
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

try:
    from PIL import Image, ImageDraw
except ModuleNotFoundError:  # pragma: no cover - surface at run_tray() time.
    Image = None  # type: ignore[assignment]
    ImageDraw = None  # type: ignore[assignment]

try:
    import pystray  # type: ignore[import-not-found]
except ModuleNotFoundError:  # pragma: no cover - surface at run_tray() time.
    pystray = None  # type: ignore[assignment]


class TrayState(enum.Enum):
    """Live operational state. Drives tooltip + icon colour."""

    IDLE = "idle"
    RECORDING = "recording"
    UPLOADING = "uploading"
    ERROR = "error"

    @property
    def tint(self) -> tuple[int, int, int]:
        # Saturated enough to survive both dark and light panel themes.
        return {
            TrayState.IDLE:      (180, 180, 180),
            TrayState.RECORDING: (220,  40,  40),
            TrayState.UPLOADING: ( 40, 100, 220),
            TrayState.ERROR:     (240, 180,  40),
        }[self]

    @property
    def tooltip(self) -> str:
        return {
            TrayState.IDLE:      "GhostScribe — idle",
            TrayState.RECORDING: "GhostScribe — recording…",
            TrayState.UPLOADING: "GhostScribe — uploading…",
            TrayState.ERROR:     "GhostScribe — error (see log)",
        }[self]


class MenuAction(enum.Enum):
    """Actions the main loop has to handle when the user picks a menu item."""

    EDIT_CONFIG    = "edit-config"
    REVEAL_CONFIG  = "reveal-config"
    RELOAD_CONFIG  = "reload-config"
    SHOW_LOG       = "show-log"
    RESTART        = "restart"
    ABOUT          = "about"
    QUIT           = "quit"


def make_icon_image(state: TrayState, size: int = 64):
    """Return a PIL ``Image`` with a filled circle tinted for ``state``.

    The background is transparent (RGBA) so the icon tracks the panel's
    background regardless of theme. ``Pillow`` is required.
    """
    if Image is None or ImageDraw is None:
        raise RuntimeError(
            "Pillow is required for tray icons; install with `pip install pillow`."
        )
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    draw = ImageDraw.Draw(img)
    # Leave a 2 px margin so the glyph doesn't touch the edge.
    margin = max(2, size // 16)
    r, g, b = state.tint
    draw.ellipse(
        (margin, margin, size - 1 - margin, size - 1 - margin),
        fill=(r, g, b, 255),
    )
    return img


@dataclass
class Tray:
    """Owning wrapper around a ``pystray.Icon``.

    Use ``set_state()`` from any thread; pystray schedules the icon swap
    on its own main-thread loop.
    """

    _icon: "pystray.Icon"  # type: ignore[name-defined]
    _state: TrayState = TrayState.IDLE
    _state_lock: threading.Lock = threading.Lock()

    def set_state(self, state: TrayState, suffix: str = "") -> None:
        with self._state_lock:
            if self._state == state and not suffix:
                # Avoid needless work; pystray also coalesces but saves a
                # Pillow round-trip on the common idle→idle case.
                return
            self._state = state
        self._icon.icon = make_icon_image(state)
        self._icon.title = (
            f"{state.tooltip} — {suffix}" if suffix else state.tooltip
        )

    def set_tooltip_suffix(self, suffix: str) -> None:
        with self._state_lock:
            state = self._state
        self._icon.title = (
            f"{state.tooltip} — {suffix}" if suffix else state.tooltip
        )

    def notify(self, title: str, message: str) -> None:
        """Try to surface a libnotify bubble; silently fall back if the
        backend doesn't support it (``pystray.Icon.notify`` handles that)."""
        try:
            self._icon.notify(message=message, title=title)
        except Exception:  # pragma: no cover - backend-dependent
            print(f"[tray] {title}: {message}", file=sys.stderr, flush=True)


def build_menu(
    on_action: Callable[[MenuAction], None],
    has_config: bool,
    can_reveal: bool,
):
    """Construct the ``pystray.Menu`` object.

    ``has_config`` enables the Reveal/Reload items; when there is no file
    on disk yet, those wouldn't do anything useful.
    """
    if pystray is None:
        raise RuntimeError("pystray is required; `pip install pystray`.")

    def _cb(action: MenuAction):
        return lambda _icon, _item: on_action(action)

    Menu = pystray.Menu
    Item = pystray.MenuItem

    return Menu(
        Item("Edit config…",               _cb(MenuAction.EDIT_CONFIG)),
        Item("Reveal config in file manager", _cb(MenuAction.REVEAL_CONFIG),
             enabled=can_reveal),
        Item("Reload now",                 _cb(MenuAction.RELOAD_CONFIG),
             enabled=has_config),
        Menu.SEPARATOR,
        Item("Show log",                   _cb(MenuAction.SHOW_LOG)),
        Item("Restart client",             _cb(MenuAction.RESTART)),
        Menu.SEPARATOR,
        Item("About GhostScribe",          _cb(MenuAction.ABOUT)),
        Item("Quit",                       _cb(MenuAction.QUIT)),
    )


def build_tray(
    on_action: Callable[[MenuAction], None],
    config_path: Optional[Path],
) -> Tray:
    """Construct a ``Tray`` wrapper ready for ``.run()`` on the main thread.

    The caller is responsible for calling ``icon.run()`` (blocks on the
    main thread; pystray requires that on GTK/Xlib) and later
    ``icon.stop()`` from the Quit handler.
    """
    if pystray is None:
        raise RuntimeError("pystray is required; `pip install pystray`.")
    menu = build_menu(
        on_action,
        has_config=config_path is not None,
        can_reveal=config_path is not None,
    )
    image = make_icon_image(TrayState.IDLE)
    icon = pystray.Icon(
        "ghostscribe",
        icon=image,
        title=TrayState.IDLE.tooltip,
        menu=menu,
    )
    return Tray(_icon=icon, _state=TrayState.IDLE, _state_lock=threading.Lock())


def run_blocking(tray: Tray) -> None:
    """Block the current (main) thread running the tray's event loop."""
    tray._icon.run()


def stop(tray: Tray) -> None:
    """Request the tray event loop to exit. Safe to call from any thread."""
    try:
        tray._icon.stop()
    except Exception:  # pragma: no cover - backend-specific shutdown races
        pass
