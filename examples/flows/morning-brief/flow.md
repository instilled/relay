---
when: "30 7 * * 1-5"
catch_up: top
steps:
  gchat:  { run: gchat.sh, env: { BRIEF_SPACE: spaces/AAAA } }  # change: your Chat space
  notion: { run: notion.py }
  github: { run: github.sh, env: { GITHUB_ORG: my-org } }       # change: your GitHub org
  days:   { run: days.sh }
  brief:
    after: [gchat, notion, github, days]
    run: { agent: claude }
  post:
    after: brief
    run: post.sh
    env: { BRIEF_SPACE: spaces/AAAA }  # change: the same space
---

Weekday mornings at 7:30, a super high-level brief of what each teammate worked on — yesterday,
or Friday through Sunday on a Monday — from Google Chat, Notion and GitHub (my-org). Four scripts
gather, one agent writes the brief, and post.sh sends it to the Google Chat space spaces/AAAA. It
heads your catch-up too. The scripts read as the accounts gws, ntn and gh are signed in to.

## brief
Below is what my teammates did yesterday (on a Monday: Friday through the weekend), from three
sources. Write a super high-level brief of what each one worked on. Merge what the sources say
about the same person (names differ across them: e.g. a GitHub login, a first name in Notion, a
full name in gchat). "(me)" is me: leave me out, but use my messages as context. Skip anyone with
nothing.

Format it for Google Chat, which knows only *bold*, _italic_ and <url|text> links — no headings,
no markdown lists, no **double stars**:

*Morning brief · {days.out}*

*<First name>*
• <one theme they worked on, a few words to one short line>
• <another; at most 4 per person, the biggest first>

Link a bullet to its source with <url|text> when one URL says it best (a PR, a Notion page);
never paste bare URLs. Leave a blank line between people. Answer with the brief alone, nothing
before or after it: it is posted as you write it.

gchat (space, sender, message):
{gchat.out}

Notion (last editor, page, url):
{notion.out}

GitHub pull requests (author, repo, state, title, url):
{github.out}
