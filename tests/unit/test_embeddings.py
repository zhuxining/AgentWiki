from collections.abc import Sequence

import fastembed
import pytest

from agentwiki.repository import embeddings


class _FixedModel:
    def __init__(self, vectors: list[list[float]]) -> None:
        self._vectors = vectors

    def embed(self, texts: Sequence[str]) -> list[list[float]]:
        return self._vectors[: len(texts)]


class FakeTextEmbedding:
    def __init__(self, *, model_name: str) -> None:
        self.model_name = model_name

    def embed(self, texts: Sequence[str]) -> list[list[float]]:
        return [[float(len(text))] for text in texts]


def test_fastembed_provider_uses_configured_text_embedding(monkeypatch) -> None:
    # TextEmbedding is imported lazily inside the provider, so patch its real module.
    monkeypatch.setattr(fastembed, "TextEmbedding", FakeTextEmbedding)
    provider = embeddings.FastEmbedProvider("local-test-model")

    assert provider.embed_documents(["hello", "world!"]) == [[1.0], [1.0]]
    assert provider.embed_query("query") == [1.0]
    assert provider._model is not None


def test_provider_normalizes_vectors_to_unit_length() -> None:
    """Unit vectors make sqlite-vec L2 distance convertible to cosine similarity."""
    provider = embeddings.FastEmbedProvider("unused")
    provider._model = _FixedModel([[3.0, 4.0], [0.0, 0.0]])  # type: ignore[assignment]

    vectors = provider.embed_documents(["a", "b"])

    assert vectors[0] == [pytest.approx(0.6), pytest.approx(0.8)]
    # A zero vector stays zero instead of producing NaN.
    assert vectors[1] == [0.0, 0.0]


def test_normalize_leaves_zero_vectors_alone() -> None:
    assert embeddings.normalize([0.0, 0.0]) == [0.0, 0.0]
    half = pytest.approx(0.70710678)
    assert embeddings.normalize([1.0, 1.0]) == [half, half]
