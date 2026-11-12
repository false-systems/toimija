//! Documentation guarantees. The Git-state thesis is part of the product, so
//! the README must state it verbatim.

const README: &str = include_str!("../README.md");

#[test]
fn readme_states_the_git_state_thesis() {
    assert!(
        README.contains(
            "We do not ask the model to understand the repo first. \
             We ask Git what state the repo is in."
        ),
        "README.md must contain the Git-state thesis sentence"
    );
}

#[test]
fn readme_states_git_is_the_substrate() {
    // The guarantee is the substance, verbatim, not a heading.
    assert!(
        README.contains("Git is not a save button here. Git is the substrate."),
        "README.md must keep the Git-substrate statement"
    );
    assert!(
        README.contains(
            "Git gives us the graph of what happened. \
             Toimija turns that graph into operational context for agents."
        ),
        "README.md must keep the graph-to-context sentence"
    );
}
