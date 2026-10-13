---
type: Type Schema
title: Journal
description: Fields of a day's quick notes (okbot design §5.9).
fields:
  author: {type: string}
  scope: {type: string, enum: [user, room]}
---
