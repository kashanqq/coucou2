"""Bitrix24 → tasks.json for Coucou. Read-only, the user's own tasks only."""


class NeedsLogin(Exception):
    """The saved session is gone: the user has to sign in again (`login`)."""
