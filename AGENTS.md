# Agent attribution

The AI agents must disclose their identity.
This includes all prose in files other than code files.
Code comments do not need to have the agent attribution.
The identity is the vendor, model version and the session ID.
The agent identity is best disclosed in a footer of a file or a comment.
For example:
Created by Claude Opus 5.5 d25c6780-611e-4140-adc9-d875cd69219a
Edited by Claude Sonnet 5.0 30058d50-c47c-4927-bd06-ba649549cba8

# Documenting in GitHub Issues

Before creating a GitHub issue, check whether the issue already exists. Prefer adding a comment to the existing issue instead of creating a new one.

# Pull Requests

Always open pull requests as drafts.
A human will mark them as ready for review.

In pull requests you should tell:
- What has been done.
- What are the main architectural assumptions.
- Why this was done.

You should also link all the relevant Github issues to the PRs.

In the pull request description you should also include a section guiding the reviewer.
Document anything that's uncertain or needs inspection.
Guide the attention and propose an order for reviewing the changes.

# Referring to Code Segments

When linking code to an issue, ensure the link stays valid over time:
- Include the **git commit hash** in the link so line numbers remain correct even if code changes
- Prefer linking to commits on **main/master**, not pull request branches (PR branches may be deleted after merge)

# Linking Issues Together

- Use the **full URL** to the issue, not just the issue number — links stay valid if the issue moves or the repo is renamed
- Link relevant issues together to form a network of related issues — this helps others find context
- Link relevant PRs to issues and issues to PRs — this makes it easy to trace which PRs address which issues

# Simple Writing

- Prefer brief and clear text.
- Any document and paragraph should be well scoped.
- You should use lead sentences.
- Yous should prefer common words over uncommon ones.
- You should use active voice where possible.
- You should write short sentences. Prefer multiple short sentences over a few long ones.

# Code Comments

Code comments should answer the question WHY. The code comments should not answer to the question WHAT.
Code comments should also be used sparingly.
It is ok to create longer comment documents to discuss architecture and link them to the relevant places in code.
Also the longer comment documents should still answer the WHY instead of the WHAT.
The DOs and the DO NOTs will help you disambiguate between the WHY and the WHAT questions.

The DOs of code comments:
- Explain non-obvious design decisions
- Document intentional error handling choices
- Explain performance or safety considerations

The DO NOTs of code comments:
- Restate what the code obviously does
- Narrate function calls or assignments
- Repeat information available from variable/function names
- Describe standard language constructs
