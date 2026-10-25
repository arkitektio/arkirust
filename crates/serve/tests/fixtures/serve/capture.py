"""Record the agent HTTP/websocket contract of a served app.

    python capture.py http://127.0.0.1:8765 out_dir/

Runs the same scenario against any server (the Python twin or the Rust port)
and writes one JSON file per observation. Frames and bodies are stored raw;
comparisons normalize ids, uuids and times.
"""

import asyncio
import json
import sys
from pathlib import Path

import httpx
import websockets

AUTH = {"authorization": "Bearer good"}
TERMINAL = {"COMPLETED", "FAILED", "CRITICAL", "CANCELLED", "INTERRUPTED"}


class Observer:
    """A websocket subscriber that records every frame."""

    def __init__(self, url: str) -> None:
        self.url = url
        self.frames: list[dict] = []
        self.init: dict | None = None

    async def __aenter__(self) -> "Observer":
        self.ws = await websockets.connect(self.url)
        await self.ws.send(json.dumps({"type": "INIT", "token": "good"}))
        self.init = json.loads(await self.ws.recv())
        self.reader = asyncio.create_task(self._read())
        return self

    async def _read(self) -> None:
        async for raw in self.ws:
            self.frames.append(json.loads(raw))

    async def __aexit__(self, *exc: object) -> None:
        self.reader.cancel()
        await self.ws.close()

    async def until_terminal(self, task: str, grace: float = 0.5, timeout: float = 10) -> list[dict]:
        """Frames of `task` up to its terminal event, plus whatever follows within `grace`."""
        loop = asyncio.get_running_loop()
        deadline = loop.time() + timeout
        while loop.time() < deadline:
            if any(f.get("task") == task and f.get("type") in TERMINAL for f in self.frames):
                break
            await asyncio.sleep(0.02)
        await asyncio.sleep(grace)
        frames, self.frames = self.frames, []
        return frames

    async def until_type(self, kind: str, timeout: float = 10) -> None:
        loop = asyncio.get_running_loop()
        deadline = loop.time() + timeout
        while loop.time() < deadline:
            if any(f.get("type") == kind for f in self.frames):
                return
            await asyncio.sleep(0.02)
        raise TimeoutError(kind)


def response(r: httpx.Response) -> dict:
    try:
        body = r.json()
    except ValueError:
        body = r.text
    keep = {k: v for k, v in r.headers.items() if k in ("www-authenticate", "content-type")}
    return {"status": r.status_code, "headers": keep, "body": body}


async def main(base: str, out: Path) -> None:
    out.mkdir(parents=True, exist_ok=True)
    ws_url = base.replace("http", "ws", 1) + "/ws"

    def save(name: str, value: object) -> None:
        (out / f"{name}.json").write_text(json.dumps(value, indent=2, sort_keys=False) + "\n")

    async with httpx.AsyncClient(base_url=base, timeout=10) as http:
        # --- auth ---------------------------------------------------------
        save("auth_http_401", response(await http.post("/assign/count_up", json={"args": {"until": 1}})))
        async with websockets.connect(ws_url) as ws:
            await ws.send(json.dumps({"type": "INIT", "token": "bad"}))
            try:
                await ws.recv()
                closed = None
            except websockets.ConnectionClosed as e:
                closed = {"code": e.rcvd.code if e.rcvd else None, "reason": e.rcvd.reason if e.rcvd else None}
        save("auth_ws_close", closed)

        async with Observer(ws_url) as obs:
            save("ws_init", obs.init)

            for name, path in [
                ("tasks_empty", "/tasks"),
                ("states_initial", "/states"),
                ("state_camera_initial", "/states/CameraState"),
                ("locks_initial", "/locks"),
                ("schemas_implementations", "/schemas/implementations"),
                ("schemas_states", "/schemas/states"),
                ("schemas_locks", "/schemas/locks"),
                ("schemas_bloks", "/schemas/bloks"),
                ("session_info", "/session_info"),
            ]:
                save(name, response(await http.get(path)))

            # --- a stateful action: LOCK, STATE_PATCH, YIELD, COMPLETED, UNLOCK
            r = await http.post("/assign/set_exposure", headers=AUTH, json={"args": {"exposure_ms": 20.0}})
            save("assign_set_exposure", response(r))
            save("frames_set_exposure", await obs.until_terminal(r.json()["task"]))

            r = await http.post("/assign", headers=AUTH, json={"interface": "add_tag", "args": {"tag": "a"}})
            save("assign_add_tag", response(r))
            save("frames_add_tag", await obs.until_terminal(r.json()["task"]))

            # --- the per-implementation route (task_id, no auth) and a generator
            r = await http.post("/count_up", json={"args": {"until": 2}})
            save("impl_count_up", response(r))
            save("frames_count_up", await obs.until_terminal(r.json()["task_id"]))

            # --- an action that raises
            r = await http.post("/assign/explode", headers=AUTH, json={"args": {}})
            save("frames_explode", await obs.until_terminal(r.json()["task"]))

            # --- step: pauses at the first pausepoint, resume releases it
            r = await http.post("/assign/pausable", headers=AUTH, json={"args": {}, "step": True})
            task = r.json()["task"]
            await obs.until_type("PAUSED")
            save("tasks_paused", response(await http.get("/tasks")))
            save("resume", response(await http.post("/resume", json={"task": task})))
            save("frames_pausable", await obs.until_terminal(task))

            # --- control routes on unknown tasks, bad bodies
            save("cancel_unknown", response(await http.post("/cancel", json={"task": "nope"})))
            save("pause_unknown", response(await http.post("/pause", json={"task": "nope"})))
            save("step_unknown", response(await http.post("/step", json={"task": "nope"})))
            save("frames_control_unknown", await obs.until_terminal("nope", grace=0.5, timeout=1))

            # --- views after the run
            save("tasks_after", response(await http.get("/tasks")))
            save("task_unknown", response(await http.get("/tasks/nope")))
            save("states_after", response(await http.get("/states")))
            save("states_filtered", response(await http.get("/states", params={"state_keys": "CameraState,Other"})))
            save("state_camera_after", response(await http.get("/states/CameraState")))
            save("locks_after", response(await http.get("/locks")))

            # --- history
            session = (await http.get("/session_info")).json()["current_session"]
            save("checkout_1", response(await http.get("/states/checkout", params={"global_revision_id": 1})))
            save("checkout_unknown_key", response(await http.get("/states/checkout", params={"global_revision_id": 1, "state_keys": "Nope"})))
            save("checkout_missing_rev", response(await http.get("/states/checkout")))
            save("segments_0_2", response(await http.get("/states/segments", params={"from_global_revision_id": 0, "to_global_revision_id": 2})))
            save("active_session_boundaries", response(await http.get("/active_session_boundaries")))
            save("session_boundaries", response(await http.get(f"/session_boundaries/{session}")))
            save("states_session_boundaries", response(await http.get("/states/session_boundaries")))
            save("task_boundaries_unknown", response(await http.get("/task_boundaries/nope")))
            save("state_at_global_1", response(await http.get(f"/state_at_global/{session}/1")))
            save("state_at_global_1_camera", response(await http.get(f"/state_at_global/{session}/1", params={"state_id": "CameraState"})))
            save("current_state_at_global_2", response(await http.get("/current_state_at_global/2")))
            save("forward_events_0", response(await http.get(f"/forward_events/{session}/0")))
            save("snapshots_around_1", response(await http.get(f"/snapshots_around/{session}/1")))

        openapi = (await http.get("/openapi.json")).json()
        components = openapi.get("components", {}).get("schemas", {})
        save(
            "openapi_action_components",
            {k: v for k, v in components.items() if k.endswith("Request") or k.endswith("Response")},
        )
        save("openapi_action_paths", {p: openapi["paths"][p] for p in ["/count_up", "/set_exposure"]})


if __name__ == "__main__":
    asyncio.run(main(sys.argv[1].rstrip("/"), Path(sys.argv[2])))
