"""The Python twin of the Rust serve test app.

Serves the same actions, state and lock with `rekuest.contrib.fastapi` so that
`capture.py` can record the HTTP/websocket contract the Rust port must match.

Run (from the arkirust repo root):

    P=~/Code/packages
    PYTHONPATH=$P/rekuest:$P/fakts:$P/rath $P/newswitch/.venv/bin/python \
        crates/rekuest/tests/fixtures/serve/twin.py 8765 /tmp/twin.db
"""

import sys
from collections.abc import Generator
from dataclasses import dataclass, field

import uvicorn
from fastapi import FastAPI, Request

from rekuest.app import AppRegistry
from rekuest.contrib.fastapi.auth import AuthenticationError
from rekuest.contrib.fastapi.routes import configure_fastapi
from rekuest.task import Task

registry = AppRegistry()


@registry.state(name="CameraState", required_locks=["camera"])
@dataclass
class CameraState:
    connected: bool = False
    exposure_ms: float = 10.0
    tags: list[str] = field(default_factory=list)


@registry.startup
def init_camera() -> CameraState:
    return CameraState(connected=True)


def set_exposure(exposure_ms: float, camera: CameraState) -> float:
    """Set Exposure

    Changes the exposure time.
    """
    camera.exposure_ms = exposure_ms
    return exposure_ms


def add_tag(tag: str, camera: CameraState) -> int:
    """Add Tag"""
    camera.tags.append(tag)
    return len(camera.tags)


def count_up(until: int) -> Generator[int, None, None]:
    """Count Up"""
    yield from range(until)


def explode() -> int:
    """Explode"""
    raise ValueError("boom")


def pausable(task: Task) -> str:
    """Pausable"""
    task.pausepoint()
    return "done"


# No getter actions: the served app already exposes state (GET /states, STATE_PATCH frames).
for function in (set_exposure, add_tag, count_up, explode, pausable):
    registry.register(function)


def expand_user(source: object) -> str:
    """Accept `Authorization: Bearer good` over HTTP and `token: good` over the websocket."""
    if isinstance(source, Request):
        if source.headers.get("authorization") == "Bearer good":
            return "tester"
        raise AuthenticationError("bad credentials")
    if getattr(source, "token", None) == "good":
        return "tester"
    raise AuthenticationError("bad credentials")


def build_app(db_file: str) -> FastAPI:
    app = FastAPI(title="twin")
    configure_fastapi(app, registry, expand_user_from_request=expand_user, db_file=db_file)
    return app


if __name__ == "__main__":
    port = int(sys.argv[1]) if len(sys.argv) > 1 else 8765
    db = sys.argv[2] if len(sys.argv) > 2 else "twin.db"
    uvicorn.run(build_app(db), host="127.0.0.1", port=port, log_level="warning")
