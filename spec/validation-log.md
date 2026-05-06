# Sigil spec validation log

Append-only log of validation runs. Each entry records the date, prompt,
model, produced program, compile/run result, and any resulting spec edits.
`scripts/validate-spec.sh` drives these runs.

Schema per entry:

```
## <ISO 8601 UTC> — P<NN> on <model>

**Prompt version:** spec/validation-prompts.md at commit <hash>
**Program (produced by the session):**
<fenced code block>
**Compile result:** ok | fail (attach compiler output)
**Run stdout:** <verbatim, fenced>
**Run exit:** <integer>
**Matches oracle:** yes | no (describe divergence)
**Spec edits prompted by this run:** <commit hashes, or "none">
**Notes:** <free-form>
```

No entries yet.
