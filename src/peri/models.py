"""Decision schema — the one contract between the analyst LLM and the engine.

The LLM returns strict JSON: {"market_view": "...", "actions": [...]}. Anything
that fails validation is a retry, then a loud AnalystError. No degraded parse.
"""

from typing import Annotated, Literal, Optional, Union

from pydantic import BaseModel, ConfigDict, Field, field_validator

Side = Literal["long", "short"]
MarginMode = Literal["cross", "isolated"]
# Was Literal[10, 20] to stop the model picking odd values. Builder dexes cap
# lower (io:ANTH at 6x, vntl at 3x), so a two-value literal made those markets
# untradeable by construction. The guard already enforces the config ceiling,
# the venue ceiling, and the isolated-liquidation band, so a bounded int is
# safe — and LOWER leverage is strictly safer: more margin posted, liquidation
# further away.
EntryLeverage = Annotated[int, Field(ge=2, le=50)]


class OpenAction(BaseModel):
    model_config = ConfigDict(extra="forbid")

    kind: Literal["open"] = "open"
    market: str                     # "BTC" (native) or "xyz:NVDA" (builder dex)
    side: Side
    conviction: float = Field(ge=0.0, le=1.0)
    # None = take the market now. A price = rest a maker limit there and wait;
    # it must be BELOW the mark for a long and ABOVE it for a short, and it
    # expires unfilled after risk.entry_expiry_secs.
    entry: Optional[float] = Field(default=None, gt=0)
    stop: float = Field(gt=0)       # absolute price, required — no stopless entries
    take_profit: float = Field(gt=0)  # absolute price, required — bracket both sides
    leverage: EntryLeverage
    margin_mode: MarginMode
    source: Literal["own", "mirror"] = "own"
    mirror_msg_id: Optional[int] = None   # telegram msg id when source == "mirror"
    rationale: str = Field(min_length=1)
    invalidation: str = Field(min_length=1)  # falsifiable condition that kills the thesis

    @field_validator("market")
    @classmethod
    def _market_nonempty(cls, v: str) -> str:
        v = v.strip()
        if not v:
            raise ValueError("market must be non-empty")
        return v


class CloseAction(BaseModel):
    model_config = ConfigDict(extra="forbid")

    kind: Literal["close"] = "close"
    market: str
    rationale: str = Field(min_length=1)


class AdjustStopAction(BaseModel):
    model_config = ConfigDict(extra="forbid")

    kind: Literal["adjust_stop"] = "adjust_stop"
    market: str
    stop: float = Field(gt=0)
    take_profit: float = Field(gt=0)
    rationale: str = Field(min_length=1)


class RememberAction(BaseModel):
    """A durable lesson. Costs nothing, risks nothing, and is the only thing
    the analyst carries from one day to the next — the model's weights never
    change, so what it writes here IS its learning."""

    model_config = ConfigDict(extra="forbid")

    kind: Literal["remember"] = "remember"
    lesson: str = Field(min_length=8, max_length=400)
    market: Optional[str] = None    # tag it when the lesson is market-specific


Action = Annotated[Union[OpenAction, CloseAction, AdjustStopAction, RememberAction],
                   Field(discriminator="kind")]


class Decision(BaseModel):
    model_config = ConfigDict(extra="forbid")

    market_view: str = ""
    actions: list[Action] = []


class ChatResponse(BaseModel):
    model_config = ConfigDict(extra="forbid")

    answer: str = Field(min_length=1)
    proposal: Optional[Action] = None
