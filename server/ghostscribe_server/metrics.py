"""Prometheus metrics definitions for the GhostScribe server."""

from prometheus_client import Counter, Histogram, REGISTRY, generate_latest, CONTENT_TYPE_LATEST

REQUEST_COUNT = Counter(
    "ghostscribe_requests_total",
    "Total transcription requests",
    ["endpoint", "status"],
)

REQUEST_DURATION = Histogram(
    "ghostscribe_request_duration_seconds",
    "End-to-end request duration (upload + inference)",
    ["endpoint"],
    buckets=[0.5, 1.0, 2.0, 3.0, 5.0, 10.0, 20.0, 30.0],
)

INFERENCE_DURATION = Histogram(
    "ghostscribe_inference_duration_seconds",
    "Whisper inference duration only",
    buckets=[0.1, 0.5, 1.0, 2.0, 3.0, 5.0, 10.0, 20.0],
)

AUDIO_DURATION = Histogram(
    "ghostscribe_audio_duration_seconds",
    "Duration of audio submitted for transcription",
    buckets=[1, 5, 10, 30, 60, 120, 300],
)


def metrics_response() -> tuple[bytes, str]:
    """Return (body, content_type) for the /metrics endpoint."""
    return generate_latest(REGISTRY), CONTENT_TYPE_LATEST
