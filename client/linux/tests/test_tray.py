"""Tests for the platform-independent slice of ``ghostscribe_client.tray``.

The pystray and Pillow imports are optional at module level so the rest
of the package can be imported on systems without those installed.
These tests skip gracefully whenever Pillow isn't available; the menu
construction tests skip when pystray itself is missing.
"""

from __future__ import annotations

import pytest

from ghostscribe_client import tray as _tray
from ghostscribe_client.tray import MenuAction, TrayState


pillow = pytest.importorskip("PIL", reason="Pillow not installed")


def test_state_tints_are_distinct() -> None:
    """If two states share a tint the user can't tell them apart at a
    glance, which defeats the whole point of the tray icon."""
    tints = {state: state.tint for state in TrayState}
    assert len(set(tints.values())) == len(TrayState)


def test_state_tooltips_mention_state_name() -> None:
    for state in TrayState:
        assert state.value in state.tooltip.lower()


@pytest.mark.parametrize("state", list(TrayState))
def test_make_icon_image_returns_rgba_of_expected_size(state: TrayState) -> None:
    img = _tray.make_icon_image(state, size=64)
    assert img.mode == "RGBA"
    assert img.size == (64, 64)


@pytest.mark.parametrize("state", list(TrayState))
def test_make_icon_image_paints_state_tint_in_centre(state: TrayState) -> None:
    img = _tray.make_icon_image(state, size=64)
    # Centre pixel must be opaque and match the state tint exactly.
    r, g, b, a = img.getpixel((32, 32))
    assert (r, g, b) == state.tint
    assert a == 255
    # Corner pixel (well outside the inscribed circle) must be transparent.
    _, _, _, corner_alpha = img.getpixel((0, 0))
    assert corner_alpha == 0


def test_menu_actions_cover_documented_set() -> None:
    """Keep the menu enum aligned with the README/plan; missing items
    here will silently break the tray UI when wiring callbacks."""
    expected = {
        "EDIT_CONFIG",
        "REVEAL_CONFIG",
        "RELOAD_CONFIG",
        "SHOW_LOG",
        "RESTART",
        "ABOUT",
        "QUIT",
    }
    assert {a.name for a in MenuAction} == expected


def test_build_menu_emits_typed_actions_in_order() -> None:
    pytest.importorskip("pystray", reason="pystray not installed")

    fired: list[MenuAction] = []
    menu = _tray.build_menu(fired.append, has_config=True, can_reveal=True)

    # Pull the user-visible menu items in declaration order, skipping
    # separators (pystray exposes them as `MenuItem` instances with no
    # text). Each callable triggers the wired callback.
    items = [it for it in menu.items if getattr(it, "text", None)]
    labels = [it.text for it in items]
    assert labels == [
        "Edit config…",
        "Reveal config in file manager",
        "Reload now",
        "Show log",
        "Restart client",
        "About GhostScribe",
        "Quit",
    ]

    for it in items:
        # The pystray callback signature is (icon, item).
        it(None, it)

    assert fired == [
        MenuAction.EDIT_CONFIG,
        MenuAction.REVEAL_CONFIG,
        MenuAction.RELOAD_CONFIG,
        MenuAction.SHOW_LOG,
        MenuAction.RESTART,
        MenuAction.ABOUT,
        MenuAction.QUIT,
    ]


def test_build_menu_disables_reveal_and_reload_when_no_config() -> None:
    pytest.importorskip("pystray", reason="pystray not installed")

    menu = _tray.build_menu(lambda _a: None, has_config=False, can_reveal=False)
    by_label = {
        it.text: it for it in menu.items if getattr(it, "text", None)
    }
    # `enabled` may be a bool or a callable; normalise.
    def _is_enabled(item) -> bool:
        e = item.enabled
        return e(item) if callable(e) else bool(e)

    assert _is_enabled(by_label["Edit config…"])
    assert not _is_enabled(by_label["Reveal config in file manager"])
    assert not _is_enabled(by_label["Reload now"])
