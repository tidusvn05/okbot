---
type: Type Schema
title: Task
description: Fields of a to-do, reminder or routine (okbot design §5.6).
fields:
  id: {type: string, required: true}
  kind: {type: string, enum: [todo, reminder, routine], required: true}
  state: {type: string, enum: [open, paused, done, cancelled], required: true}
  scope: {type: string, enum: [user, room]}
  due: {type: string}
  remind_at: {type: string}
  repeat: {type: string}
  timezone: {type: string}
  deliver_to: {type: string}
  author: {type: string}
  created: {type: string}
---
