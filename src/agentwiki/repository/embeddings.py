"""Optional local embedding providers for semantic document search."""

from collections.abc import Iterable, Sequence
from typing import Protocol, cast

from fastembed import TextEmbedding


class _EmbeddingModel(Protocol):
    def embed(self, texts: Sequence[str]) -> Iterable[Sequence[float]]: ...


class EmbeddingProvider(Protocol):
    """Small synchronous contract used by the local SQLite index."""

    @property
    def model_name(self) -> str: ...

    def embed_documents(self, texts: Sequence[str]) -> list[list[float]]: ...

    def embed_query(self, text: str) -> list[float]: ...


class FastEmbedProvider:
    """Lazy FastEmbed provider; model loading happens on first embedding call."""

    def __init__(self, model_name: str = "BAAI/bge-small-en-v1.5") -> None:
        self._model_name = model_name
        self._model: _EmbeddingModel | None = None

    @property
    def model_name(self) -> str:
        return self._model_name

    def _get_model(self) -> _EmbeddingModel:
        if self._model is None:
            self._model = cast(_EmbeddingModel, TextEmbedding(model_name=self._model_name))
        return self._model

    def embed_documents(self, texts: Sequence[str]) -> list[list[float]]:
        vectors = self._get_model().embed(list(texts))
        return [[float(value) for value in vector] for vector in vectors]

    def embed_query(self, text: str) -> list[float]:
        return self.embed_documents([text])[0]
