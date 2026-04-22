"""Integration tests for the Prometheus ``/metrics`` endpoint."""

from __future__ import annotations

import re


def _count_line(body: str, metric: str, labels: dict[str, str]) -> float:
    """Return the sample value for ``metric{labels...}`` in a Prometheus text body.

    Returns 0.0 when the exact labelled series hasn't been created yet.
    """
    # Build a regex that matches the labels in any order.
    label_alt = "|".join(
        re.escape(f'{k}="{v}"') for k, v in labels.items()
    )
    pat = re.compile(
        rf"^{re.escape(metric)}\{{(?:{label_alt})(?:,(?:{label_alt}))*\}}\s+(\S+)$",
        re.MULTILINE,
    )
    m = pat.search(body)
    return float(m.group(1)) if m else 0.0


def test_metrics_endpoint_serves_prometheus_text(make_client) -> None:
    with make_client() as client:
        r = client.get("/metrics")
    assert r.status_code == 200
    assert r.headers["content-type"].startswith("text/plain")
    assert "ghostscribe_requests_total" in r.text


def test_request_counter_increments_on_success(
    make_client, silence_wav_bytes
) -> None:
    labels = {"endpoint": "/v1/en", "status": "200"}
    with make_client() as client:
        before = _count_line(
            client.get("/metrics").text, "ghostscribe_requests_total", labels
        )
        client.post(
            "/v1/en",
            files={"audio": ("x.wav", silence_wav_bytes, "audio/wav")},
        )
        after = _count_line(
            client.get("/metrics").text, "ghostscribe_requests_total", labels
        )
    assert after == before + 1.0


def test_request_counter_increments_on_503(make_client, silence_wav_bytes) -> None:
    labels = {"endpoint": "/v1/en", "status": "503"}
    with make_client() as client:
        before = _count_line(
            client.get("/metrics").text, "ghostscribe_requests_total", labels
        )
        client.app.state.engine.ready = False
        client.post(
            "/v1/en",
            files={"audio": ("x.wav", silence_wav_bytes, "audio/wav")},
        )
        after = _count_line(
            client.get("/metrics").text, "ghostscribe_requests_total", labels
        )
    assert after == before + 1.0
