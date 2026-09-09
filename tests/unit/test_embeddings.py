from collections.abc import Sequence

from agentwiki.repository import embeddings


class FakeTextEmbedding:
    def __init__(self, *, model_name: str) -> None:
        self.model_name = model_name

    def embed(self, texts: Sequence[str]) -> list[list[float]]:
        return [[float(len(text))] for text in texts]


def test_fastembed_provider_uses_configured_text_embedding(monkeypatch) -> None:
    monkeypatch.setattr(embeddings, "TextEmbedding", FakeTextEmbedding)
    provider = embeddings.FastEmbedProvider("local-test-model")

    assert provider.embed_documents(["hello", "world!"]) == [[5.0], [6.0]]
    assert provider.embed_query("query") == [5.0]
    assert provider._model is not None
