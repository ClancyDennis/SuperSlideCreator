"""Auth strategies. API key now, AAD seam for later.

To migrate to AAD:
  from azure.identity import DefaultAzureCredential
  cred = DefaultAzureCredential()
  token = cred.get_token("https://cognitiveservices.azure.com/.default").token
  return AadAuth(token)
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Protocol


class AuthStrategy(Protocol):
    def headers(self) -> dict[str, str]: ...


@dataclass(frozen=True)
class ApiKeyAuth:
    api_key: str

    def headers(self) -> dict[str, str]:
        return {"api-key": self.api_key}


@dataclass(frozen=True)
class AadAuth:
    bearer_token: str

    def headers(self) -> dict[str, str]:
        return {"Authorization": f"Bearer {self.bearer_token}"}
