---
type: Guide
title: Getting started with the knowledge base
description: How operators add their organization's documents to the bot's knowledge base.
tags: [onboarding]
lang: en
status: stable
updated: 2026-10-12
generated: {by: okbot-init, at: "2026-10-12"}
---

# Getting started with the knowledge base

Put your organization's knowledge in this `knowledge/` folder as Markdown files, one topic per file,
each with a `title` and a one-sentence `description`. Spreadsheets go in `data/`.

Useful commands (from the bot's directory):

- `okbot kb lint` checks the documents.
- `okbase adopt` converts plain Markdown folders; `okbase import` brings in PDFs and Office files.
- `okbase view` shows the knowledge base in a browser.

The bot re-reads changed files within a few seconds.
