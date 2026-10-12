---
type: Type Schema
title: Memory
description: Fields of memory notes the bot writes about a chat or the whole team.
fields:
  author: {type: string}
  source: {type: string, enum: [chat, web, admin]}
---
