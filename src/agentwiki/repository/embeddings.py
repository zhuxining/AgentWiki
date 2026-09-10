"""Optional local embedding providers for semantic document search."""

from collections.abc import Iterable, Sequence
import math
from typing import Protocol, cast


class _EmbeddingModel(Protocol):
    def embed(self, texts: Sequence[str]) -> Iterable[Sequence[float]]: ...


class EmbeddingProvider(Protocol):
    """Small synchronous contract used by the local SQLite index."""

    @property
    def model_name(self) -> str: ...

    def embed_documents(self, texts: Sequence[str]) -> list[list[float]]: ...

    def embed_query(self, text: str) -> list[float]: ...


def normalize(vector: Sequence[float]) -> list[float]:
    """Return the unit-length form of ``vector``.

    sqlite-vec returns L2 distances; on unit vectors ``cos = 1 - L2²/2``, which is what
    lets the vector leg apply a comparable similarity threshold. A zero vector is
    returned unchanged so it stays non-matching rather than becoming NaN.
    """
    norm = math.sqrt(sum(value * value for value in vector))
    if not norm or not math.isfinite(norm):
        return [float(value) for value in vector]
    return [float(value) / norm for value in vector]


class FastEmbedProvider:
    """Lazy FastEmbed provider; model loading happens on first embedding call.

    Every vector is L2-normalized on the way out so distances are comparable across
    queries and can be turned into a cosine similarity.
    """

    DEFAULT_MODEL = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2"

    def __init__(self, model_name: str = DEFAULT_MODEL) -> None:
        self._model_name = model_name
        self._model: _EmbeddingModel | None = None

    @property
    def model_name(self) -> str:
        return self._model_name

    def _get_model(self) -> _EmbeddingModel:
        if self._model is None:
            # Imported lazily: importing fastembed pulls in onnxruntime, which writes a
            # session file into the current working directory as an import side effect.
            try:
                from fastembed import TextEmbedding

                self._model = cast(_EmbeddingModel, TextEmbedding(model_name=self._model_name))
            except (ImportError, OSError, RuntimeError, ValueError) as exc:
                # A missing model must degrade to keyword search, never break retrieval.
                raise RuntimeError(
                    f"embedding model {self._model_name!r} is unavailable: {exc}"
                ) from exc
        return self._model

    def embed_documents(self, texts: Sequence[str]) -> list[list[float]]:
        vectors = self._get_model().embed(list(texts))
        return [normalize([float(value) for value in vector]) for vector in vectors]

    def embed_query(self, text: str) -> list[float]:
        return self.embed_documents([text])[0]
