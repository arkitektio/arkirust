import json
from typing import Optional
from arkitekt import App
from mikro.api.schema import ArrayDataset
from mikro.arkitekt import mikro as mikro_service

app = App("hello", "0.1.0")
app.service(mikro_service)

@app.action
def greet(name: str, times: int = 1) -> str:
    """Greet someone

    Says hello, possibly several times.

    Args:
        name: Who to greet
        times: How often

    Returns:
        The greeting
    """
    return name * times

@app.action
def rescale(image: ArrayDataset, factors: list[float], label: Optional[str] = None) -> tuple[ArrayDataset, int]:
    """Rescale an image"""
    ...

@app.action
def no_doc(flag: bool, lookup: dict[str, int]) -> None:
    pass

inp = app.registry.to_implement_agent_input(name="hello:0.1.0")
print(json.dumps(inp.model_dump(mode="json", by_alias=False, exclude_unset=True), indent=2))
