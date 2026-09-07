"""Telegram ingest: store every group message raw (the LLM reads them verbatim),
flag allowlisted callers, and wake the engine immediately on a caller message.
No parsing here — parsing was the old bot; the brain reads.

Pictures count as messages. A caller who posts a chart and types nothing has
still made a call, and until an `ImageReader` was attached here that message
was discarded before it ever reached the ledger."""

import asyncio
from typing import Optional


class Feed:
    def __init__(self, state, callers: list[str], wake: asyncio.Event,
                 reader=None):
        self.state = state
        self.callers = {c.lower() for c in callers}
        self.wake = wake
        self.reader = reader   # peri.vision.ImageReader, or None to stay blind

    def ingest(self, msg_id: int, ts: float, sender: Optional[str], text: str,
               image_desc: Optional[str] = None) -> bool:
        """Store one message. Returns True if it was new AND from a caller
        (i.e. the engine should cycle now)."""
        if not text and not image_desc:
            return False
        is_caller = bool(sender) and sender.lower() in self.callers
        fresh = self.state.add_tg_message(msg_id, ts, sender, text, is_caller,
                                          image_desc=image_desc)
        if fresh and is_caller:
            self.wake.set()
            return True
        return False

    @staticmethod
    def _has_media(message) -> bool:
        return bool(getattr(message, "photo", None) or getattr(message, "document", None))

    async def read_media(self, message) -> Optional[str]:
        """Transcribe an attached picture, or None if there is nothing to read.

        Downloads into memory — these are screenshots, and a bounded one at
        that; nothing from a group the operator does not control touches disk.
        A failure here returns None and the message still lands with its
        caption: the analyst is told the picture exists and could not be read,
        never handed a guess about what was in it."""
        if self.reader is None:
            return None
        from peri.vision import IMAGE_MIME
        mime = "image/jpeg"
        if getattr(message, "photo", None) is None:
            doc_mime = getattr(getattr(message, "document", None), "mime_type", None)
            if doc_mime not in IMAGE_MIME:
                return None      # a video, a pdf, a sticker: not for the eye
            mime = doc_mime
        try:
            data = await message.download_media(file=bytes)
        except Exception as exc:  # noqa: BLE001 — a dead download is not a dead feed
            print(f"[peri] image download failed: {exc!r}", flush=True)
            return None
        # the reader's OWN pool, never asyncio.to_thread: see ImageReader.pool
        return await asyncio.get_running_loop().run_in_executor(
            self.reader.pool, self.reader.read_image,
            data or b"", (message.text or "")[:200], mime)

    def ingest_news(self, source: str, msg_id: int, ts: float, text: str) -> bool:
        """Store one news-channel post (no wake — news informs the next cycle)."""
        if not text:
            return False
        fresh = self.state.add_news_item(source, msg_id, ts, text.strip()[:500])
        if fresh:
            print(f"[peri] news {source}: {text.strip()[:70]!r}", flush=True)
        return fresh

    def attach(self, client, group_id: int) -> None:
        """Wire new and edited group messages into the same upsert path."""
        from telethon import events

        async def _ingest_event(event, edited: bool = False):
            sender = await event.get_sender()
            uname = getattr(sender, "username", None)
            message = event.message
            stamp = message.edit_date if edited and message.edit_date else message.date
            # An edit re-reads nothing: the stored description is kept by
            # COALESCE, so editing a caption costs no vision call.
            desc = None if edited else await self.read_media(message)
            text = message.text or ""
            if desc is None and not text and not edited and self._has_media(message):
                # An empty message would be dropped, so leave a marker — but say
                # which it was. "could not read" when nothing was attempted is a
                # lie the analyst has no way to see through.
                text = ("[posted an image the system could not read]"
                        if self.reader is not None
                        else "[posted an image; image reading is off]")
            self.ingest(message.id, stamp.timestamp(), uname, text, image_desc=desc)

        @client.on(events.NewMessage(chats=group_id))
        async def _on_message(event):  # pragma: no cover — telethon glue
            await _ingest_event(event)

        @client.on(events.MessageEdited(chats=group_id))
        async def _on_edit(event):  # pragma: no cover — telethon glue
            await _ingest_event(event, edited=True)

    def attach_news(self, client, channels: list[str]) -> None:
        """Wire configured telegram news channels into ingest_news()."""
        if not channels:
            return
        from telethon import events

        @client.on(events.NewMessage(chats=channels))
        async def _on_news(event):  # pragma: no cover — telethon glue
            chat = await event.get_chat()
            name = getattr(chat, "username", None) or getattr(chat, "title", "channel")
            self.ingest_news(name, event.message.id,
                             event.message.date.timestamp(),
                             event.message.text or "")
