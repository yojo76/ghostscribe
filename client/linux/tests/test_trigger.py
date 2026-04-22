"""Unit tests for trigger parsing in ghostscribe_client.__main__."""

from __future__ import annotations

import pytest
from pynput import keyboard, mouse

from ghostscribe_client.__main__ import (
    KeyTrigger,
    MouseTrigger,
    OneKeyLinuxTrigger,
    parse_one_key_trigger,
    parse_trigger,
)


def test_mouse_x2() -> None:
    t = parse_trigger("mouse:x2")
    assert isinstance(t, MouseTrigger)
    assert isinstance(t.button, mouse.Button)
    assert t.label == "mouse:x2"


def test_mouse_forward_alias_resolves_same_as_x2() -> None:
    a = parse_trigger("mouse:x2")
    b = parse_trigger("mouse:forward")
    assert isinstance(a, MouseTrigger) and isinstance(b, MouseTrigger)
    assert a.button == b.button


def test_mouse_unknown_button_raises() -> None:
    with pytest.raises(ValueError, match="unknown mouse button"):
        parse_trigger("mouse:nonsense")


def test_key_single_special() -> None:
    t = parse_trigger("key:f12")
    assert isinstance(t, KeyTrigger)
    assert t.target == keyboard.Key.f12
    assert t.modifiers == ()
    assert t.label == "key:f12"


def test_key_single_char() -> None:
    t = parse_trigger("key:g")
    assert isinstance(t, KeyTrigger)
    assert isinstance(t.target, keyboard.KeyCode)
    assert t.target.char == "g"


def test_key_chord_ctrl_g() -> None:
    t = parse_trigger("key:ctrl+g")
    assert isinstance(t, KeyTrigger)
    assert len(t.modifiers) == 1
    assert keyboard.Key.ctrl_l in t.modifiers[0]
    assert keyboard.Key.ctrl_r in t.modifiers[0]
    assert t.label == "key:ctrl+g"


def test_key_multi_modifier() -> None:
    t = parse_trigger("key:ctrl+shift+space")
    assert isinstance(t, KeyTrigger)
    assert len(t.modifiers) == 2
    assert t.target == keyboard.Key.space


def test_key_modifier_case_insensitive() -> None:
    t = parse_trigger("key:CTRL+SHIFT+space")
    assert isinstance(t, KeyTrigger)
    assert len(t.modifiers) == 2


def test_key_unknown_modifier_raises() -> None:
    with pytest.raises(ValueError, match="unknown modifier"):
        parse_trigger("key:hyper+g")


def test_key_unknown_target_raises() -> None:
    with pytest.raises(ValueError, match="unknown key"):
        parse_trigger("key:ctrl+nonexistent")


def test_missing_colon_raises() -> None:
    with pytest.raises(ValueError, match="invalid trigger"):
        parse_trigger("mouse_x2")


def test_unknown_kind_raises() -> None:
    with pytest.raises(ValueError, match="unknown trigger kind"):
        parse_trigger("gesture:swipe")


def test_empty_value_raises() -> None:
    with pytest.raises(ValueError):
        parse_trigger("key:")


# --------------------------------------------------------------------------- #
# parse_one_key_trigger                                                       #
# --------------------------------------------------------------------------- #


def test_one_key_empty_is_disabled() -> None:
    assert parse_one_key_trigger("") is None
    assert parse_one_key_trigger("   ") is None


def test_one_key_ctrl_returns_trigger() -> None:
    t = parse_one_key_trigger("key:ctrl")
    assert isinstance(t, OneKeyLinuxTrigger)
    assert keyboard.Key.ctrl in t.key_family
    assert keyboard.Key.ctrl_l in t.key_family
    assert keyboard.Key.ctrl_r in t.key_family
    assert t.label == "key:ctrl"


def test_one_key_alt_returns_trigger() -> None:
    t = parse_one_key_trigger("key:alt")
    assert isinstance(t, OneKeyLinuxTrigger)
    assert keyboard.Key.alt in t.key_family


def test_one_key_function_keys() -> None:
    for n in range(1, 25):
        fkey = getattr(keyboard.Key, f"f{n}", None)
        if fkey is None:
            continue  # platform may not expose all F-keys
        t = parse_one_key_trigger(f"key:f{n}")
        assert isinstance(t, OneKeyLinuxTrigger)
        assert fkey in t.key_family, f"f{n} missing from key_family"


def test_one_key_is_case_insensitive() -> None:
    t1 = parse_one_key_trigger("KEY:CTRL")
    t2 = parse_one_key_trigger("key:ctrl")
    assert t1 is not None and t2 is not None
    assert t1.key_family == t2.key_family
    assert t1.label == "key:ctrl"  # label is normalised to lowercase


def test_one_key_rejects_missing_prefix() -> None:
    with pytest.raises(ValueError, match="must start with 'key:'"):
        parse_one_key_trigger("ctrl")


def test_one_key_rejects_chord() -> None:
    with pytest.raises(ValueError, match="cannot be a chord"):
        parse_one_key_trigger("key:ctrl+g")


def test_one_key_rejects_letters_digits_shift() -> None:
    for bad in ["key:a", "key:g", "key:1", "key:shift", "key:f0", "key:f25"]:
        with pytest.raises(ValueError):
            parse_one_key_trigger(bad)
