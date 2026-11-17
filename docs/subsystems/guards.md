# marv-guards (`tests/guards/`)

One test per rule in `docs/SECURITY.md` (#8, Marv issue #53's guard suite). It's a workspace test crate (`publish = false`), not part of any shipped crate, so the rules read as one list. CI runs it with `cargo test --workspace` and real bubblewrap (`.github/workflows/ci.yml` installs it). Tests that run bwrap skip, with a line on stderr, where it can't create namespaces.

Each guard's doc comment says how it was proven: the guard was reverted, and the test failed. The reverts were run as a script against this tree (one revert at a time, file restored after). Rules whose code isn't written yet have a guard marked `#[ignore = "needs …"]` with the body it will run.

| `SECURITY.md` rule | Test (`tests/guards/tests/…`) | Status |
|---|---|---|
| One static binary: no `.env`/`bunfig.toml` loader | `launch.rs::no_dotenv_loader_is_linked_in` | Proven |
| Config only from `~/.marv` or `$MARV_CONFIG_DIR` in the real env | `launch.rs::the_config_folder_comes_only_from_the_real_environment` | Proven |
| A project's `.marv/config.json` sets only its allow-list (`formatter`, `checks`, `"yolo": false`); every other key, and one added later, is named in the note and applies nothing | `boot.rs::a_project_config_sets_only_its_allow_list` | Proven (a deny-list in place of the allow-list; `"model"` added to it) |
| … an untrusted project's config only narrows (its formatter map and `"checks": true` ignored, named) | `boot.rs::an_untrusted_projects_config_only_narrows` | Proven (the untrusted arm dropped) |
| … an untrusted project's `.marv/checks` don't run (checks off, each named; `marv -p` says so) | `boot.rs::an_untrusted_projects_checks_dont_run` | Proven (`hold_checks` dropped) |
| … a project's narrowing reaches `marv -p`, its widening (`"yolo": true`, `approve`) doesn't | `boot.rs::a_projects_narrowing_reaches_marv_p_and_its_widening_doesnt` | Proven (headless without `apply`) |
| `printable()`: CSI/OSC/DCS, C0/C1, bidi | `launch.rs::printable_strips_escapes_controls_and_bidi` | Proven |
| … including headless text mode | `launch.rs::headless_text_output_is_printable` | Ignored: needs #11 |
| macOS (Seatbelt, #166): Mach/XPC doors, sockets, the runtime folder but the plug, the closed home, writes, protected names and hard links, no IP without network, host processes and setuid, the environment, process lifetime | `seatbelt.rs` (14 guards, `require_sandbox!`, macOS only) | Run on the macOS job with `MARV_REQUIRE_SANDBOX=1`. Proven on macOS CI by removing the rule (branch `proof/166-seatbelt-rules`, PR #178): the home deny, the hidden-entry regex, `~/Library`, the protected write denies, the pid kill before the group's; the link refusals hold with the link rules removed (Seatbelt refuses `link()` from a source it can't read or write), so each link rule is checked on its own in `marv-sandbox`'s `seatbelt_probe.rs` (`each_link_rule_refuses_on_its_own`: the read or write deny in front of it taken out, the link must still be refused, and is made once the rule goes too; #183), and that OS behaviour is pinned by `link_is_refused_from_a_source_denied_for_reads_or_writes` |
| Read-only system, empty `/run` but the mesh plug | `sandbox.rs::system_is_read_only_and_run_is_empty_but_for_the_mesh_plug` | Proven (real bwrap) |
| No network unless approved, cleared env, own PID namespace | `sandbox.rs::no_network_cleared_environment_and_own_pid_namespace` | Proven (real bwrap) |
| Stand-in home, toolchains read-only, credentials masked | `sandbox.rs::the_home_is_a_stand_in_with_masked_credentials` | Proven (real bwrap) |
| Project writable, `.git` and protected paths read-only (at any level) | `sandbox.rs::the_project_is_writable_but_git_and_protected_paths_are_read_only` | Proven (real bwrap) |
| Protected files a command *creates* (at any depth outside `node_modules`/`target`/`.git`) | `sandbox.rs::protected_files_a_command_creates_are_found_and_reverted` | Proven (real bwrap) |
| … through the `bash` tool | `sandbox.rs::bash_reverts_protected_files_its_command_created` | Proven (real bwrap) |
| Process door: can't write outside the project | `process.rs::a_process_cant_write_outside_the_project` | Proven (real bwrap, #24) |
| Process door: can't change `.git` | `process.rs::a_process_cant_change_the_projects_git` | Proven (real bwrap, #24) |
| Process door: can't read the real home | `process.rs::a_process_cant_read_the_real_home` | Proven (real bwrap, #24) |
| Process door: no host `127.0.0.1` without `network: true` | `process.rs::a_process_cant_reach_the_hosts_localhost_without_network` | Proven (real bwrap, #24) |
| Process door: forwarded ports on the host's `127.0.0.1` only | `process.rs::a_forwarded_port_is_on_the_hosts_127_0_0_1_only` | Proven (real bwrap, #24) |
| Process door: dies on stop | `process.rs::a_process_dies_on_stop` | Proven (real bwrap, #24) |
| Process door: dies when Marv is SIGKILLed | `process.rs::a_process_dies_when_marv_is_sigkilled` | Proven (real bwrap, #24) |
| Process door: a start asks even in yolo; a no starts nothing | `process.rs::a_start_asks_even_in_yolo_and_a_no_starts_nothing` | Proven (#24) |
| Process door: a subagent can't start one | `process.rs::a_subagents_context_cant_start_one` | Proven (#24) |
| The allocator (runtime-kernel §16.10): charged to every budget above, ask or pause at a limit, no core move, page out on a core switch, budgets only the person's, Usage events without text | `allocator.rs` (10 guards) | Proven (one revert at a time, each restored) |
| Only the person's own agent types run on another provider | `cores.rs::a_projects_agent_type_cant_name_another_provider` | Proven |
| Protected paths ask even in yolo (every pattern, any level, through a symlink) | `sandbox.rs::protected_paths_ask_even_in_yolo` | Proven |
| `package.json` lifecycle scripts | `sandbox.rs::package_json_lifecycle_scripts_ask_even_in_yolo` | Proven (#4) |
| Secrets need approval to read | `sandbox.rs::reading_a_secret_asks` | Ignored: no secret-read gate yet |
| Secrets masked in the sandbox | `sandbox.rs::secrets_are_masked_in_the_sandbox` | Ignored: no project secret masks yet |
| Every path checked after `realpath` | `sandbox.rs::no_tool_follows_a_symlink_out_of_the_project` | Proven |
| … a dangling symlink out of the project or into `.git` | `sandbox.rs::no_write_through_a_dangling_symlink_out_of_the_project` | Proven |
| … skill files and folders | `sandbox.rs::skill_files_and_folders_stay_inside_after_realpath` | Proven |
| … instruction files: project, parent and nested `AGENTS.md`/`CLAUDE.md` | `sandbox.rs::instruction_files_stay_inside_after_realpath` | Proven (#62) |
| … and inside the project, never a secret or a `node_modules`/`target`/`.git` target | `sandbox.rs::instruction_files_never_read_secrets_or_skipped_folders` | Proven (#62, two reverts) |
| Saved tool outputs: read_file/grep read only the session's folder; no writes, bash can't see it | `sandbox.rs::only_reading_tools_reach_the_sessions_saved_outputs_and_nothing_else_outside` | Proven (#61; two reverts; bash part real bwrap) |
| Results tagged; outside text fenced, never Marv's voice | `provenance.rs::outside_text_is_tagged_and_fenced_as_data` | Proven |
| Web/MCP/mesh/image content taints | `provenance.rs::outside_content_taints_the_session` | Proven |
| Authority from the person and Marv only: every tool declares its source; results raised to it | `untrusted_text.rs::every_tool_declares_a_source_class` | Proven |
| The GitHub door's output is outside text (fenced, taints) | `untrusted_text.rs::github_door_output_is_fenced_and_taints` | Proven (fake gh) |
| A networked command's output taints; offline it's the project's | `untrusted_text.rs::network_command_output_taints_and_sandboxed_offline_output_doesnt` | Proven (two locks: both reverted) |
| An untrusted project's instructions are quoted; its skills and agents aren't offered | `untrusted_text.rs::an_untrusted_projects_instructions_load_fenced_and_offer_no_skills` | Proven (two reverts) |
| A changed instruction file asks for trust again | `untrusted_text.rs::a_changed_instruction_file_asks_for_trust_again` | Proven |
| An outside source's error results are fenced (MCP errors are the server's) | `untrusted_text.rs::tainting_error_results_are_fenced` | Proven (two reverts) |
| Fence attributes are escaped | `untrusted_text.rs::fence_attributes_are_escaped` | Proven |
| MCP descriptions in the deferred note and tool_search are fenced | `untrusted_text.rs::mcp_descriptions_in_announce_and_tool_search_are_fenced` | Proven |
| mesh_list's receptacle descriptions are fenced | `untrusted_text.rs::mesh_list_descriptions_are_fenced` | Proven |
| A tainted subagent's report is fenced | `untrusted_text.rs::a_tainted_subagents_report_is_fenced` | Proven |
| Tainted: a memory write asks every time | `untrusted_text.rs::a_tainted_conversation_asks_once_for_a_memory_write` | Proven |
| A tainted compaction's summary says so in fixed words | `untrusted_text.rs::a_tainted_compaction_says_so_in_fixed_words` | Proven |
| `/clear` ends the cleared conversation's taint | `untrusted_text.rs::clear_ends_the_cleared_conversations_taint` | Proven |
| No suggestion after GitHub or networked output | `untrusted_text.rs::suggestions_stay_off_after_github_or_network_output` | Proven |
| An unsandboxed command's output is network output | `untrusted_text.rs::an_unsandboxed_commands_output_is_network_output` | Proven (two locks: both reverted) |
| An outside tool's own error is fenced and taints | `untrusted_text.rs::an_outside_tools_own_error_is_fenced_and_taints` | Proven |
| Closing tags with invisible or lookalike characters are defused | `untrusted_text.rs::closing_tags_hidden_by_invisible_characters_are_defused` | Proven |
| A subfolder instruction file counts only once its contents are trusted | `untrusted_text.rs::a_subfolder_instruction_file_counts_only_once_its_contents_are_trusted` | Proven |
| A trusted skill's changed file is quoted | `untrusted_text.rs::a_trusted_skills_file_that_changed_since_is_quoted` | Proven |
| A resumed conversation is tainted by what its calls show | `untrusted_text.rs::a_resumed_conversation_is_tainted_by_what_its_calls_show` | Proven |
| A subagent's taint reaches its parent; its report is fenced | `untrusted_text.rs::a_subagents_taint_reaches_its_parent_and_its_report_is_fenced` | Proven (two reverts) |
| The session's own tools declare their sources | `untrusted_text.rs::the_session_tools_declare_their_sources_too` | Proven |
| `marv -p` never writes a memory in a tainted run | `headless_taint.rs::headless_never_writes_a_memory_in_a_tainted_run` | Proven |
| Tainted: network bash asks | `provenance.rs::tainted_network_bash_must_ask` | Proven |
| Tainted: every MCP call asks, `readOnlyHint` too | `provenance.rs::tainted_mcp_calls_ask_even_read_only_ones` | Proven (two locks: both reverted) |
| Tainted: a read of your own remote app runs unasked, its writes ask every time | `provenance.rs::your_own_remote_apps_reads_run_unasked_once_tainted_and_its_writes_ask_every_time` | Proven |
| Tainted: web requests to a new prefix; "always" covers a prefix, not the site | `provenance.rs::tainted_web_fetch_to_a_new_prefix_asks` | Proven (#5; three reverts) |
| Tainted: mesh calls on unapproved wires | `provenance.rs::mesh_egress_is_default_deny_and_asks_once_tainted` | Proven |
| Tainted: writes to protected paths | `sandbox.rs::protected_paths_ask_even_in_yolo` (`must_ask`) | Proven |
| `must_ask` never answered by "always" | `provenance.rs::always_never_answers_must_ask` | Proven |
| Approvals show the full payload | `provenance.rs::approvals_show_the_full_payload` | Proven |
| A network approval lists scripts written this session | `provenance.rs::a_network_approval_lists_the_scripts_it_runs` | Ignored: not built yet |
| A mesh message that wakes an idle Marv is fenced and taints; its turn's egress asks (#78) | `mesh.rs::a_woken_turn_reads_its_message_fenced_and_tainted_and_egress_asks` | Proven |
| Subscriptions (#159): a provider never gets another provider's key | `subscriptions.rs::a_providers_key_never_goes_to_another_provider` | Proven |
| Marv's own secrets (provider keys, MCP tokens) are redacted from every tool result in `run_tool` | `secrets.rs::run_tool_redacts_configured_secrets_from_every_result` | Proven |
| … end to end: an unsandboxed `marv -p` that `cat`s config.json and mcp.json sends, streams and saves neither | `secrets.rs::an_unsandboxed_run_that_cats_marvs_config_never_sends_its_keys` | Proven |
| Subscriptions: a plan's usage limit is a hard stop (one request, never retried, no other key) | `subscriptions.rs::plan_limit::a_plans_limit_is_a_hard_stop` | Proven |
| Sign in with ChatGPT: the loopback listener is 127.0.0.1 only, needs the state, takes one callback | `subscriptions.rs::sign_in_listener::the_sign_in_listener_is_loopback_only_needs_the_state_and_takes_one_callback` | Proven |
| Sign in with ChatGPT: an idle local connection doesn't stall the callback | `subscriptions.rs::sign_in_listener::an_idle_connection_doesnt_stall_the_sign_in` | |
| Sign in with ChatGPT: the token goes only to api.openai.com, whatever baseUrl says | `subscriptions.rs::the_chatgpt_token_is_never_sent_to_another_host` | |
| Sign in with ChatGPT: the listener times out and stops on cancel | `subscriptions.rs::sign_in_listener::the_sign_in_listener_times_out` | |
| Sign in with ChatGPT: tokens never in `Debug` output | `subscriptions.rs::sign_in_listener::the_sign_in_tokens_never_show_in_debug_output` | |
| Radio (#124): a project's config can't turn it (or wake) on; the startup note | `radio.rs::a_projects_config_cant_turn_the_radio_on` | Proven |
| Radio: off unless the user's own config turns it on (no env variable, no flag) | `radio.rs::the_radio_is_off_unless_the_users_config_turns_it_on` (+ `marv-cli`'s `cli::tests::no_flag_turns_the_radio_on`) | Proven |
| Radio: a Marv with it off refuses joins and sends no presence | `radio.rs::a_marv_with_the_radio_off_refuses_joins_and_sends_no_presence` | Proven |
| Radio: a join from a process that isn't the named Marv is refused (pid) | `radio.rs::a_join_from_a_process_that_isnt_the_named_marv_is_refused` | Proven (python3 for the other process) |
| Radio: a frame that names its own sender is refused (the sender is the link) | `radio.rs::a_frame_that_names_its_own_sender_is_refused` | Proven (two reverts) |
| Radio: presence has exactly the listed fields | `radio.rs::presence_has_exactly_the_listed_fields` | Proven (two reverts) |
| Radio: presence never reaches a model and doesn't taint | `radio.rs::presence_never_reaches_a_model_and_doesnt_taint` | Proven (two reverts) |
| Radio: sandboxed commands can't see it | `radio.rs::sandboxed_commands_cant_see_the_radio` | Proven (real bwrap) |
| Radio: a killed peer shows lost and its socket is swept | `radio.rs::a_killed_peer_shows_lost_and_its_socket_is_swept` | Proven (two reverts; the peer is this test binary run again) |
| Radio: a silent peer greys, then drops | `radio.rs::a_silent_peer_greys_then_drops` | Proven (two reverts) |
| Radio (phase 2): radio text is fenced and taints before the request that carries it | `radio.rs::radio_text_is_fenced_and_taints_before_the_request_that_carries_it` | Proven |
| Radio: a body can't close the radio fence | `radio.rs::a_body_cant_close_the_radio_fence` | Proven |
| Radio: egress after radio text asks, even in yolo | `radio.rs::egress_after_radio_text_asks_even_in_yolo` | Proven (real bwrap) |
| Radio: nothing on the radio can answer an approval | `radio.rs::nothing_on_the_radio_can_answer_an_approval` (+ `marv-cli`'s `mesh_wake::a_radio_message_never_wakes_while_an_approval_waits`) | Proven (two reverts) |
| Radio: `by: user` is shown but changes nothing | `radio.rs::by_user_is_shown_but_changes_nothing` | Proven |
| Radio: the model can't turn the radio on | `radio.rs::the_model_cant_turn_the_radio_on` | Proven |
| Radio: a turn started by the radio asks for what yolo would have allowed | `radio.rs::a_turn_started_by_the_radio_asks_for_what_yolo_would_have_allowed` | Proven (two reverts) |
| Radio: subagents have no radio | `radio.rs::subagents_have_no_radio` | Proven |
| Radio: `marv -p` is never on the radio | `radio.rs::marv_p_is_never_on_the_radio` | Proven (the real binary) |
| Radio: `radio_tx`'s spec is constant and stays after the radio goes off | `radio.rs::radio_tx_spec_is_constant_and_stays_after_radio_off` | Proven |
| Radio: history stays append-only when radio notes arrive | `radio.rs::history_stays_append_only_when_radio_notes_arrive` | Proven |
| "Send now": the person's message during a turn never lands between a tool call and its result | `messages.rs::a_message_sent_during_a_turn_never_lands_between_a_tool_call_and_its_result`, `messages.rs::a_history_that_ends_mid_call_keeps_the_message_waiting` | Proven (the `at_step_boundary` check in `can_deliver` removed: the second fails) |
| … and never makes two user turns in a row | `messages.rs::a_message_sent_now_never_makes_two_user_turns_in_a_row` | Proven (the merge into an unsent last user turn replaced by a push) |
| Radio: a turn its person joined with "send now" never quotes its reply in the report | `radio.rs::a_turn_its_person_joined_never_quotes_its_reply_in_the_report` | Proven (`person_joined` passed as false) |
| Radio: oversize messages are refused, not cut | `radio.rs::oversize_messages_are_refused_not_cut` | Proven (two reverts) |
| Radio: per-sender rates apply at the receiver | `radio.rs::per_sender_rates_apply_at_the_receiver` | Proven |
| Radio: broadcasts never wake | `radio.rs::broadcasts_never_wake` | Proven |
| Radio: radio wakes are limited per sender and overall | `radio.rs::radio_wakes_are_limited_per_sender_and_overall` | Proven (two reverts) |
| Radio: a slow peer never stalls the sender | `radio.rs::a_slow_peer_never_stalls_the_sender` | Proven |
| Radio: every frame is logged without its text | `radio.rs::every_frame_is_logged_without_its_text` | Proven |
| Radio (phase 3): `ack`, `done` and `failed` never get a reply or a report | `radio.rs::ack_done_and_failed_never_get_a_reply_or_a_report` | Proven |
| Radio: a request chain stops at two hops | `radio.rs::a_request_chain_stops_at_two_hops` | Proven |
| Radio: a report wakes the requester only for a request from a user's turn | `radio.rs::a_report_wakes_the_requester_only_for_a_request_from_a_users_turn` | Proven |
| Radio: a request gets one ack and one report (also when the model replied itself) | `radio.rs::a_request_gets_one_ack_and_one_report` | Proven |
| Radio: the report is built without a model call | `radio.rs::the_report_is_built_without_a_model_call` (+ `marv-cli`'s `tests/radio.rs::a_request_is_acked_handled_and_reported_back`) | Proven |
| Radio: failed reports for stopped, declined and errored turns | `radio.rs::failed_reports_for_stopped_declined_and_errored_turns` | Proven |
| Radio: a request waits while busy and is read only at a turn's start | `radio.rs::a_request_waits_while_busy_and_is_read_only_at_a_turns_start` | Proven |
| Radio: open requests survive compaction and resume | `radio.rs::open_requests_survive_compaction_and_resume` | Proven |
| Radio: a Marv leaving mid-request reports failed | `radio.rs::a_marv_leaving_mid_request_reports_failed` | Proven |
| Radio: a turn that handles a request asks for what yolo would have allowed (marv-rs's decision, past spec §8.4) | `radio.rs::a_turn_that_handles_a_request_asks_even_in_yolo` | Proven |
| Radio: `radio_tx`'s wait returns the report fenced, tainting first | `radio.rs::radio_tx_waits_for_the_report_fenced_and_tainting` | Proven |
| Radio: a pane send to a Marv on the radio is refused, with the radio hint | `radio.rs::pane_send_to_a_marv_on_the_radio_is_refused_with_the_radio_hint` | Proven (a fake `herdr`) |
| Compaction (#63): taint survives the v1 and v2 summaries, overflow recovery and its level-3 drop, in memory and saved | `compaction.rs::taint_survives_compaction` | Proven (a taint reset in `install`; the saved taint dropped) |
| Saved pages: written before the history is replaced; a failed write keeps it | `compaction.rs::a_page_out_writes_swap_before_replacing_the_history` | Proven |
| Saved pages: the page table comes from the turns, not the summary | `compaction.rs::the_page_table_is_written_by_marv` | Proven |
| `recall`: a recalled web result is fenced and taints again | `compaction.rs::a_recalled_page_keeps_its_provenance` | Proven |
| `recall`: only the conversation's own pages; no id or path; none for a subagent | `compaction.rs::recall_reads_only_its_own_conversation` | Proven |
| `recall`: the tool list is byte-stable across a page-out | `compaction.rs::the_tool_list_is_byte_stable_across_a_page_out` | Proven |
| Saved pages: a symlinked swap folder is refused | `compaction.rs::a_symlinked_swap_folder_is_refused` | Proven |
| Saved pages: never overwritten, numbered under the lock | `compaction.rs::pages_are_never_overwritten` | Proven (the lock removed; `next_page` from the table only) |
| Saved pages: a path in the table is at most 200 characters | `compaction.rs::a_page_table_path_is_capped_at_200_characters` | Proven |
| Runtime-kernel §11.2: the context budget is the person's (not a project's, not the model's), never past 85% of the window, a planted saved value dropped | `memory_manager.rs::the_context_budget_is_set_only_by_the_person` | Proven (the saved value's `MIN_COMPACT_TOKENS` filter; the window's cap in `threshold`) |
| Runtime-kernel §11.4: a page-out's `compacted` event is sizes and a fixed word, never text | `memory_manager.rs::compacted_events_carry_sizes_never_text` | Proven (the `log_compacted` call dropped) |
| Runtime-kernel §11.2: a card's context numbers are cut to `MAX_CARD_TOKENS`, never in the fenced words | `memory_manager.rs::a_cards_context_numbers_are_numbers_within_limits` | Proven (the cut in `Card::context`) |
| Saved pages: aged out, but never a live session's | `compaction.rs::saved_pages_age_out_but_never_a_live_sessions` | Proven (no hold; no cleanup) |
| Subagents don't inherit network/web/mesh approvals | `provenance.rs::subagents_inherit_taint_but_no_egress_approvals` | Proven |
| … the config's pre-approved scopes too (#98) | `approvals.rs::subagents_dont_inherit_config_scopes` | Proven |
| Pre-approved scopes answer their own scopes (#98) | `approvals.rs::config_approve_answers_its_scopes_without_asking` | Proven (session level) |
| Pre-approved never cover tainted egress (#98) | `approvals.rs::config_approve_never_covers_tainted_egress` | Proven (the `must_ask` half reverted) |
| Pre-approved never cover protected paths (#98) | `approvals.rs::config_approve_never_covers_protected_paths` | Proven (same revert) |
| Pre-approved never cover lifecycle scripts (#98) | `approvals.rs::config_approve_never_covers_lifecycle_scripts` | Proven (same revert) |
| Pre-approved never grant a mesh wire (#98) | `approvals.rs::config_approve_never_grants_a_mesh_wire` | Proven (construction: `wire_approval` is `must_ask`+`once`) |
| In the home folder, "always" on a file change still asks the next time | `approvals.rs::in_the_home_folder_always_on_a_file_change_still_asks_the_next_time` | Proven (`home_file_write` dropped from the registry's `once` → failed) |
| … the request is `once` (no "always" offered) | `approvals.rs::in_the_home_folder_a_file_change_request_is_once` | Proven (same revert) |
| … config `"approve": ["files"]` doesn't cover it | `approvals.rs::in_the_home_folder_config_approve_files_doesnt_cover_a_file_change`, `headless_config_approve_files_in_the_home_folder_is_refused` | Proven (same revert; the second through `marv -p`) |
| … a worktree subagent's automatic yes doesn't cover a `once` request | `approvals.rs::a_subagents_automatic_yes_doesnt_cover_a_once_request` | Proven (`!request.once` dropped from `SubagentApprover` → failed) |
| A project under the home keeps "always" for file changes | `approvals.rs::a_project_under_the_home_keeps_always_for_file_changes` | Proven (`home_file_write` true for every project → failed) |
| `marv -p --approve files` still writes in the home folder | `approvals.rs::headless_approve_files_in_the_home_folder_still_writes` | Behaviour lock (the chosen rule) |
| Writable entries refused at startup (#98) | `sandbox.rs::refused_writable_entries_are_refused_at_startup` | Proven (any hidden top-level home entry: the rule dropped → failed) |
| A writable folder that is or holds a git repo refused (#98 review) | `sandbox.rs::git_repositories_are_refused_as_writable_entries` | Proven (the `.git` checks dropped → failed) |
| A writable folder writable, nothing else new (#98) | `sandbox.rs::the_writable_folder_is_writable_and_nothing_else_new_is` | Proven (real bwrap; the session merge dropped → failed) |
| A worktree subagent's `.git` is read-only; Marv commits for it | `worktree.rs::a_worktree_subagents_git_is_read_only_and_marv_commits_for_it` | Proven (real bwrap, #12) |
| … Marv's commit trusts nothing the worktree says | `worktree.rs::marvs_commit_trusts_nothing_the_worktree_says` | Proven (#12) |
| github: what gh can never do never reaches it, whatever the approver says | `github.rs::what_can_never_run_never_reaches_gh_and_isnt_asked` | Proven (fake gh) |
| github: writes and sensitive reads ask in yolo; a no runs nothing | `github.rs::writes_and_sensitive_reads_ask_in_yolo_and_a_no_runs_nothing` | Proven |
| github: a token never reaches the model, events, saved session or trajectory | `github.rs::a_token_in_ghs_output_never_reaches_the_model_the_events_the_saved_session_or_the_trajectory` | Proven |
| github: a subagent only reads | `github.rs::a_subagent_reads_but_its_write_is_refused_even_with_the_approver_saying_yes` | Proven |
| github: not offered where gh is logged out | `github.rs::not_offered_where_gh_is_logged_out_not_even_to_subagents` | Proven |
| github: `marv -p` needs `--approve github` for a write | `github.rs::headless_a_write_is_refused_without_approve_github` | Proven (the marv binary) |
| host: asks in yolo, as the host door, `once` | `host.rs::asks_in_yolo_with_the_host_door_and_once` | Proven (real bwrap) |
| host: "always" never covers a second call | `host.rs::never_uses_always_two_calls_two_asks` | Proven (real bwrap) |
| host: refused before any ask without `host_commands` | `host.rs::a_session_without_host_commands_refuses_it_before_its_approver_is_asked` | Proven (real bwrap) |
| host: a subagent can't | `host.rs::a_subagent_cant_even_where_the_session_has_the_door` | Proven (real bwrap) |
| host: `marv -p` refuses it with every `--approve` name | `host.rs::headless_refuses_it_with_every_approve_name` | Proven (two layers, both reverted) |
| herdr: the sandbox sees neither Herdr's variables nor its socket | `herdr.rs::a_sandboxed_command_sees_neither_herdrs_variables_nor_its_socket` | Proven (real bwrap) |
| herdr: what the door reads is fenced and taints | `herdr.rs::what_the_door_reads_back_is_fenced_and_taints_the_conversation` | Proven (fake herdr) |
| herdr: run and spawn ask every time (yolo, "always", every pre-approved scope) | `herdr.rs::run_and_spawn_ask_every_time_even_in_yolo_after_always_and_with_pre_approved_scopes` | Proven |
| herdr: refused before any ask without `host_commands` | `herdr.rs::a_session_without_host_commands_refuses_run_and_spawn_before_asking` | Proven |
| herdr: `marv -p` refuses run and spawn with every `--approve` name | `herdr.rs::headless_refuses_run_and_spawn_with_every_approve_name` | Proven (two layers, both reverted; the marv binary) |
| herdr: a subagent's context refuses run and spawn | `herdr.rs::a_subagents_context_refuses_run_and_spawn` | Proven |
| herdr: a spawned worker gets no more than its parent | `herdr.rs::a_spawned_worker_never_gets_more_than_its_parent` | Proven (two reverts) |
| herdr: the peer limit holds before any ask | `herdr.rs::the_peer_limit_holds_before_anyone_is_asked` | Proven |
| herdr: a light action in a woken turn gets no "always" | `herdr.rs::a_light_action_in_a_woken_turn_gets_no_always` | Proven |
| herdr: names and output reach the terminal through `printable()` | `herdr.rs::herdr_names_and_output_reach_the_terminal_only_through_printable` | Proven |
| A turn a mesh message wakes runs with automatic approval off: yolo asks there (#74) | `mesh.rs::an_inbox_woken_turn_asks_for_what_yolo_would_allow` | Proven |
| mesh (kernel 2b): every wire check goes through `Switchboard::authorize` | `mesh_policy.rs::every_wire_check_goes_through_authorize` | Proven (two reverts) |
| mesh (kernel 2b): seeing an agent in `mesh_list` grants nothing | `mesh_policy.rs::seeing_an_agent_in_mesh_list_grants_nothing` | Proven |
| mesh (kernel 3): being in the registry grants nothing; only listing and filling code reads it | `mesh_policy.rs::being_in_the_registry_grants_nothing` | Proven (`authorize` admitting a registered Marv's inbox) |
| kernel (3): the event log never holds a message's text (a marker sent, delivered, refused and waking is nowhere in it) | `event_log.rs::the_event_log_never_holds_a_messages_text` | Proven (the body put into the logged id) |
| kernel (§18.1, B1): a boot is logged once per conversation as hashes and names, never words; `/clear` boots again with the same hashes | `boot_kernel.rs::a_boot_is_logged_once_as_hashes_and_names_never_words` | Proven (logged every turn; the prompt passed as its hash) |
| kernel (§17.1, D1): a device switched off for a chat (`devices_off`) isn't offered: the github door, an MCP server | `boot_kernel.rs::a_device_switched_off_for_a_chat_isnt_offered` | Proven (`sdk.rs` ignoring it, for github and for `mcp:`) |
| kernel (3): the event log is 0600 in a 0700 folder in the user's config folder, never a project | `event_log.rs::the_event_log_is_0600_in_a_0700_folder_in_the_users_config_never_a_project` | Proven (the folder's 0700 dropped) |
| kernel (3): a project's `.marv` can't redirect the event log (relative config folder refused, symlinks not written through) | `event_log.rs::a_projects_marv_folder_cant_redirect_the_event_log` | Proven (a relative config folder accepted) |
| mesh (kernel 2b): a project's `policies.json` is never read | `mesh_policy.rs::a_projects_policies_json_is_never_read` | Proven |
| mesh (kernel 2b): where rules aren't honored (`marv -p`), a matching rule changes nothing; rules read and honored only where they should be | `mesh_policy.rs::where_rules_arent_honored_a_matching_rule_changes_nothing` | Proven (two reverts) |
| mesh (kernel 2b, D2): under a rule, sends to the person's own Marvs ask nothing, and nothing else is excused | `mesh_policy.rs::under_a_rule_sends_to_the_persons_own_marvs_ask_nothing_and_nothing_else_is_excused` | Proven (two reverts) |
| mesh (kernel 2b): only the person's own tools (the desktop's Teams, `marv mesh allow`) make a rule | `mesh_policy.rs::only_the_persons_own_tools_make_a_rule` | Proven |
| mesh (kernel 2b): a rule's messages are fenced and tainting, stop waking at hop 2, and end when it expires | `mesh_policy.rs::a_rules_messages_are_fenced_tainting_bounded_and_end_with_it` | Proven |
| mesh (kernel 2b): a rule never covers a Herdr pane, a `call`, a worker or a shim; a tainted send to a shim asks | `mesh_policy.rs::rules_never_cover_a_pane_a_call_or_a_shim` | Proven (both main-inbox checks) |
| mesh (kernel 2b): the model can't set a message's hop | `mesh_policy.rs::the_model_cant_set_a_messages_hop` | Proven |
| mesh (kernel 2b): a claimed hop is ignored on a shim or a Marv plug | `mesh_policy.rs::a_claimed_hop_is_ignored_on_a_shim_or_a_marv_plug` | Proven |
| mesh (kernel live UX): by default an idle TUI wakes only for a rule's wire; `"wake": false` stops every wake | `mesh_policy.rs::the_default_wake_is_a_rules_wire_only_and_off_is_off` | Proven (two reverts) |
| mesh (kernel live UX): `mesh_wait` asks nobody, says who sent how many, and the mail reaches the model only fenced and tainting at the next step | `mesh.rs::mesh_wait_says_who_and_the_mail_comes_fenced_at_the_next_step` | Proven (drained the inbox into the wait's result: failed) |
| mesh (kernel live UX): the hop stop is told to the sender (mesh_send result), the person (waiting line) and the event log (`wake_skipped`, no words) | `mesh_policy.rs::at_the_hop_stop_the_sender_the_person_and_the_log_are_told` | Proven (dropped the send note: failed; dropped `log_waiting`'s append: failed) |
| mesh (coordination): a team's wires wake past the hop stop within a per-chat budget; a dropped wake tells the sender, the person and the log; a report-woken turn reports to nobody | `mesh_policy.rs::a_teams_wires_wake_past_the_hop_stop_within_a_budget_and_a_dropped_wake_says_who_acts`, `roles.rs::a_turn_a_report_woke_sends_no_report_back` | Proven (the team arm dropped: no wake; `set_team_pause` dropped: no sender note) |
| event log (kernel live UX): a declined wire is a `wire_refused` and an unwired send a `not_wired` refusal, ids and fixed words only | `event_log.rs::the_event_log_never_holds_a_messages_text` | Proven (dropped the decline log: failed; no refusal category: failed) |
| mesh (kernel live UX): `mesh_list` says which Marvs a rule covers, and that grants nothing | `mesh_policy.rs::mesh_list_says_which_marvs_a_rule_covers_and_that_grants_nothing` | Proven (matched the rule against the caller instead of the peer: failed) |
| mesh (kernel live UX, review P2): a person's turn runs out of rounds after `MAX_LEAD_ROUNDS` rule-wire replies; its sends then arrive but wake nobody until the person's next message | `mesh_policy.rs::a_persons_turn_runs_out_of_rounds_and_its_sends_stop_waking` | Proven (dropped the `rounds_out` wake check: failed; dropped `count_round`: failed) |
| mesh (kernel live UX, review P3): a turn a rule's message woke under the default reads only rules' wires; hand-wired mail waits for the person's turn | `mesh_policy.rs::a_rule_woken_turn_leaves_hand_wired_mail_for_the_persons_turn` | Proven (woke with `rules_only: false`: failed) |
| event log (kernel live UX, review P3): a waiting message is logged as a skipped wake once, across a session reload | `event_log.rs::a_waiting_message_is_logged_once_across_a_reload` | Proven (dropped the `first_skip_logs` filter: failed) |
| mesh (kernel chat card): only the person's typed words become a chat card's title and `now`: never plan steps, tool output, the model's replies, a mesh message or a woken turn | `chat_card.rs::model_tool_and_woken_text_never_become_the_card` | Proven (`MarvSession::card` over every transcript message: failed; a woken turn's line as `Role::User`: failed) |
| mesh (kernel chat card): a card's words reach another Marv's model only when a rule the person made covers that Marv, fenced as data, and don't taint it (decided: B) | `chat_card.rs::a_cards_words_reach_only_covered_marvs_fenced_not_tainting` | Proven (dropped the `covering(..)` condition from the card filter: failed; `CARD_WORDS_TAINT` back to `true`: failed) |
| services: no grant, a woken turn asks for everything | `services.rs::no_grant_means_a_woken_turn_asks_for_everything` | Proven (the yolo test without a grant) |
| services: a grant runs only what yolo would | `services.rs::a_grant_runs_only_what_yolo_would` | Proven (two reverts: `auto_safe`, `!home_write`) |
| services: tainted egress asks under a grant | `services.rs::tainted_egress_asks_under_a_grant` | Proven (`!must_ask`) |
| services: a grant ignores remembered always and URL prefixes | `services.rs::a_grant_ignores_remembered_always_and_url_prefixes` | Proven (two reverts) |
| services: a model, a mesh message or a project can't make a grant | `services.rs::a_model_a_mesh_message_or_a_project_cant_make_a_grant` | Proven (a planted `.set_cap(`) |
| services: a revoked, expired or spent grant asks at the next call | `services.rs::a_revoked_expired_or_spent_grant_asks_at_the_next_call` | Proven (`live` without `grant_for`) |
| interrupts (kernel §15): only stop, revoke/expiry/deadline (unattended) and shutdown preempt | `interrupts.rs::only_stop_revoke_and_shutdown_preempt_a_turn` | Proven (a `Mail`/`Wire` arm in `preempts`) |
| interrupts: a person's message runs before a queued wake | `interrupts.rs::a_person_message_always_runs_before_a_queued_wake` | Proven (`InterruptQueue::pop` by arrival only) |
| interrupts: a peer can send only mail | `interrupts.rs::a_peer_can_send_only_mail` | Proven (a `SignalBy::Peer` variant) |
| timers: `wake_me` needs a grant and stays inside its limits | `interrupts.rs::wake_me_needs_a_grant_and_stays_inside_its_limits` | Proven (`FileSelfTimers::set` without the `live()` check) |
| timers: a recurring timer is made only by the person | `interrupts.rs::a_recurring_timer_is_made_only_by_the_person` | Proven (`Timer::check` without the chat-recurring arm) |
| timers: a timer's note arrives fenced, never as the person's words | `interrupts.rs::a_timer_note_arrives_fenced_never_as_the_persons_words` | Proven (`for_model` without `fenced_note`) |
| timers: an unattended turn stops at its deadline | `interrupts.rs::an_unattended_turn_stops_at_its_deadline` | Proven (`with_deadline` without the interrupt and cancel: timed out) |
| timers (review): only a granted unattended turn has a deadline | `interrupts.rs::only_a_granted_unattended_turn_has_a_deadline` | Proven (`turn_deadline` always `Some`) |
| timers (review): a chat's timer is void under another grant | `interrupts.rs::a_chats_timer_is_void_under_another_grant` | Proven (`may_fire` taking any grant) |
| timers (review): a timer keeps its hop; `wake_me` asks when tainted | `interrupts.rs::a_timer_keeps_its_hop_and_wake_me_asks_when_tainted` | Proven (two reverts: the `Woken::Timer` hop arm, `uses_network_in`) |
| interrupts: interrupt, signal and timer events carry no text | `interrupts.rs::interrupt_events_carry_no_text` | Proven (`KernelEvent` without `deny_unknown_fields`) |
| visible OS (kernel §20.8): Activity never reaches a model (the registry `mesh_list` reads keeps no door's `live`; the list a model gets prints none, from the registry or a scan; no mesh tool reads it) | `visible_os.rs::activity_never_reaches_a_model` | Proven (`info_of` with `live: Some(..)`; the door computing `live` unasked); covers a `door_state` door too |
| visible OS (§20.3): the log view has no message text, an edited log's names are cleaned, every line a fixed sentence | `visible_os.rs::the_log_view_has_no_message_text` | Proven (`Names::from_log` without `clean_name`; `Notifications::push` without it); a secret-looking name, a timer's note |
| visible OS (§20.2): a terminal Marv appears with its state (idle, waiting for you, idle) and its live numbers | `visible_os.rs::a_terminal_marv_appears_with_its_state` | Proven (the session's `set_door` dropped: "running") |
| visible OS (§20.8): signals come only from the person (no door method signals; no mesh tool is a signal; a logged signal is the person's) | `visible_os.rs::signals_come_only_from_the_person` | Proven (a door arm for `marv/pause`; a `Pause` variant in `KernelCommand`) |
| visible OS (§20.9): process stats only for PIDs Marv started (refused: this process, init, an outside process, a released PID) | `visible_os.rs::proc_reads_only_marvs_own_pids` | Proven (`procstats::stats` without the own-PID check; without the start-tick comparison) |
| services: a grant never lifts a wake limit or the hop stop | `services.rs::a_grant_never_lifts_a_wake_limit_or_the_hop_stop` | Proven (the hop stop dropped) |
| services: a hand-wired or radio wake runs without the grant | `services.rs::a_hand_wired_or_radio_wake_runs_without_the_grant` | Proven (the grant on every woken turn) |
| services: every granted call is logged without its input | `services.rs::every_granted_call_is_logged_without_its_input` | Proven (the label as scope key) |
| services: a notification never reaches a model | `services.rs::a_notification_never_reaches_a_model` | Proven (a planted note); the desktop's OS notifications (separate change) still to add |
| services: a paused grant's chat still wakes, ungranted (its edit asks), and the person is told (review P2.2) | `services.rs::a_paused_grant_wakes_ungranted_and_the_person_is_told` | Proven (three reverts: the notice dropped, the paused wake not started, a paused grant woken as live) |
| mesh (kernel board, §19.3): board text is fenced (closing tag defused) and taints the reader | `board.rs::board_text_is_fenced_and_taints_the_reader` | Proven (`BoardTool::run` without `Provenance::Mesh`: failed; `board::fenced` without `defuse_closing_tag`: failed) |
| mesh (kernel board, §19.1): the author is stamped by the kernel; a model clears only its own | `board.rs::the_author_is_stamped_by_the_kernel` | Proven (`board::post` stamping a fixed author instead of the switchboard's own name: failed; `board::clear` clearing all: failed) |
| mesh (kernel board, §19.1): no rule, no board (also outside the rule's folders, and in `marv -p`) | `board.rs::no_rule_no_board` | Proven (`Switchboard::board_rules` without `covers`: failed) |
| mesh (kernel board, §19.4): a post after a `chat_read` asks the person, `once` | `board.rs::a_post_after_chat_read_asks` (yolo on, under a send rule) | Proven (`BoardTool::own_marvs_only` without the `read_chat` check: failed) |
| mesh (kernel board, §19.4): after a `chat_read`, an unattended turn's post asks too, a live grant notwithstanding | `board.rs::a_post_after_chat_read_asks_in_an_unattended_turn_with_a_grant` | Proven (`BoardTool::own_marvs_only` without the `read_chat` check: failed) |
| mesh (kernel board, §19.4, review P2-1): a post runs unasked only where the chat could `mesh_send` under the rule (a read rule asks; the receiving end of a one-way rule asks, every time) | `board.rs::a_post_asks_where_the_chat_couldnt_send_under_the_rule` | Proven (`board_rules` marking every rule `sends`: failed) |
| mesh (kernel board, review P2-2): a rule that doesn't cover the chat is refused by each tool, its board untouched | `board.rs::a_rule_that_doesnt_cover_the_chat_is_refused` | Proven (`board::chosen` taking an uncovered asked rule: failed) |
| mesh (kernel board, §19.5): a post wakes nobody | `board.rs::a_post_wakes_nobody` | A control: no guard code to remove (a post touches only the board's file); it fails if a post ever mails or wakes |
| mesh (kernel board, §19.2): revoking the rule deletes its board, and a post can't bring it back | `board.rs::revoking_the_rule_deletes_its_board` | Proven (`PolicyStore::revoke` without `delete_board`: failed) |
| mesh (kernel board, §19.2): the limits hold (280 characters, 5 claims and 10 notes an author, 50 entries with only the poster's own pushed off, 24 h) | `board.rs::limits_hold` | Proven (the claims check, the full board dropping the poster's own oldest (made the board's oldest), `live_at` in `read`, the length check: each, failed) |
| mesh (kernel board, §19.6): board events carry no text or paths | `board.rs::board_events_carry_no_text_or_paths` | Proven (the `BoardPost` log call dropped: failed) |
| mesh (kernel board): an edit tool can't write a board (outside the project; `.marv/**` protected in a home project) | `board.rs::an_edit_tool_cant_write_a_board` | Proven (`rule_for_relative` without the `.marv` rule: failed) |
| mesh (kernel chat_read): a `send` rule alone never allows a read; a `read` rule does, only within its folders, checked by the reader and again by the target's host | `chat_read.rs::a_send_rule_alone_never_allows_a_read_and_a_read_rule_only_within_its_folders` | Proven (`match_rule` matching any mode: failed; `chat_read::serve` without its rule check: failed) |
| mesh (kernel chat_read): a tool's input or output is never served (R1), also from a TUI's transcript tool entry (its label and summary) | `chat_read.rs::tool_outputs_and_inputs_are_never_served` | Proven (`snapshot_of` serving a `Role::Tool` entry's label and summary: failed; serving the conversation's tool results: failed) |
| mesh (kernel chat_read): a private chat refuses every read, also under a rule (R3) | `chat_read.rs::a_private_chat_refuses_every_read_under_a_rule_too` | Proven (the host's source skipping `private`, also the desktop's `ResidentSource`: failed) |
| mesh (kernel chat_read): the reader is tainted by a read and its egress outside the mesh asks | `chat_read.rs::the_reader_is_tainted_and_its_egress_asks` | Proven (`ChatReadTool` without `Provenance::Mesh`: failed) |
| mesh (kernel chat_read): refused reads are logged, and the log never holds served text | `chat_read.rs::refused_reads_are_logged_and_the_log_never_holds_text` | Proven (the reader's own no-rule refusal unlogged: failed) |
| mesh (kernel chat_read): `marv -p` and subagents have no `chat_read` | `chat_read.rs::marv_p_and_subagents_have_no_chat_read` | Proven (`SessionBuilder` setting `chat_read`: failed; `ChatReadTool` in `SubagentPlug::tools`: failed) |
| mesh (kernel chat_read): the card a read serves holds only person-typed text | `chat_read.rs::the_card_a_read_serves_holds_only_person_typed_text` | Proven (`MarvSession::card` over every transcript message: failed) |
| mesh (kernel chat_read): read-and-relay: a turn the person didn't start (a mesh wake) can't chat_read | `chat_read.rs::a_woken_turn_cant_read_another_chat_to_relay_it` | Proven (`ChatReadTool::run` without its `no_auto_approval` check: failed) |
| mesh (kernel chat_read): read-and-relay: in a person's turn, a mesh send after a read asks the person (D2's no-ask lost), and the flag is saved with the conversation | `chat_read.rs::a_send_after_a_read_asks_the_person` | Proven (`MeshTool::own_marvs_only` without the `read_chat` check: failed) |
| sleeping chats (kernel 1): tainted before unload, tainted after a wake reload; its egress asks in yolo; a woken turn on it asks `once` | `sleeping_chats.rs::a_chat_tainted_before_unload_is_tainted_after_a_wake_reload_and_its_egress_asks` | Proven (dropped the resume's taint restore: tainted check failed, and the fetch asked without `mustAsk`) |
| herdr: `/peers stop` types nothing into a busy or asking agent | `herdr.rs::stopping_a_peer_types_nothing_into_an_agent_thats_busy_or_asking` | Proven |
| doors: a `gh`/`herdr` planted in the project or a relative PATH entry is never run | `herdr.rs::a_door_program_planted_in_the_project_is_never_run` | Proven |
| herdr: nothing starting with `-` reaches the CLI | `herdr.rs::nothing_starting_with_a_dash_reaches_the_herdr_cli` | Proven (four reverts) |
| herdr: a worker spawned from a tainted conversation starts tainted | `herdr.rs::a_worker_spawned_from_a_tainted_conversation_starts_tainted` | Proven (two reverts; the marv binary) |
| herdr: a tainted prompt asks every time and says whose words | `herdr.rs::a_prompt_in_a_tainted_conversation_asks_every_time_and_says_whose_words` | Proven (two reverts) |
| `marv -p`: tainted egress refused with `--approve web` alone | `headless_taint.rs::headless_refuses_tainted_egress_with_approve_alone` | Proven (the marv binary) |
| `marv -p`: `--approve-tainted` opts in; untainted `--approve web` unchanged | `headless_taint.rs::headless_approve_tainted_allows_tainted_egress`, `headless_approve_web_still_allows_untainted_egress` | Controls |
| `marv -p`: a protected-path write needs `--approve-protected` | `headless_taint.rs::headless_refuses_protected_writes_without_approve_protected` | Proven (the marv binary) |
| Project MCP servers need trust; hash covers entry and files | `mcp.rs::project_servers_need_trust_covering_entry_and_files` | Proven |
| `npx`/`uvx` outside the project | `mcp.rs::package_runners_start_outside_the_project` | Proven |
| Server `instructions` ignored | `mcp.rs::server_instructions_never_leave_the_parser` | Proven (a source check until the manager connects, M7) |
| The MCP client advertises no capabilities (no sampling, roots or elicitation) | `devices.rs::the_mcp_client_advertises_no_capabilities` | Proven (`{"sampling": {}}` sent) |
| `/mcp add` never changes a running conversation's tool list (byte-identical specs; the next conversation gets it) | `devices.rs::mcp_add_never_changes_a_running_conversations_tool_list` | Proven (`offered` reading the manager's live tools; `tool_search`/`mcp_call` given the live manager) |
| A server's `list_changed` never changes a running tool list | `devices.rs::list_changed_never_changes_a_running_tool_list` | Proven (the manager listing again after the notification) |
| No tool installs an MCP server (writes `mcp.json`, trusts, or starts one with `McpManager::add`) | `devices.rs::no_tool_installs_a_driver` | Proven (an `add_personal_server` call, and an `mcp.add(` call, added to `subagent.rs`) |
| No tool writes Marv's config; only the shells' setup code | `boot.rs::no_tool_writes_marvs_config` | Proven (a `ConfigStore` save added to `subagent.rs`) |
| A subagent's context is narrower, never wider (memory, host, taint in and back, yolo) | `boot.rs::a_subagents_context_is_narrower_never_wider` | Proven (memory given; a fresh taint; `no_auto_approval` dropped) |
| A subagent's tools are within its parent's, its tokens are the parent's | `boot.rs::a_subagent_is_offered_tools_within_its_parents_and_spends_its_parents_budget` | Proven (every built-in offered; `sink.usage` dropped) |
| A Herdr-spawned Marv gets no more than its parent: a new process with the person's own config, held out of yolo unless the parent is in yolo (`MARV_YOLO=off`), tainted when the parent was (`MARV_TAINTED=1`) | `herdr.rs::a_spawned_worker_never_gets_more_than_its_parent`, `herdr.rs::a_worker_spawned_from_a_tainted_conversation_starts_tainted` | Proven (see herdr.rs) |
| Hidden web text: hiding classes | `web.rs::text_hidden_by_a_class_is_dropped` | Proven (#5) |
| Hidden web text: off-screen and zero-size styles | `web.rs::text_hidden_off_screen_or_in_a_zero_size_box_is_dropped` | Proven (#5) |
| Hidden web text: zero-width and tag characters | `web.rs::invisible_characters_never_reach_the_model` | Proven (#5) |
| Hidden web text: HTML comments in raw Markdown | `web.rs::html_comments_in_raw_markdown_are_dropped` | Proven (#5) |
| File changes: an edit after another session's write is refused until the file is read again (two processes) | `stale_writes.rs::an_edit_after_another_sessions_write_is_refused_until_reread` | Proven |
| File changes: an existing file the session never read can't be overwritten or edited; a new file can | `stale_writes.rs::an_existing_file_never_read_cant_be_overwritten` | Proven |
| File changes: the check and the write are one step under a cross-process lock | `stale_writes.rs::the_check_and_write_are_atomic_across_processes` | Proven |
| File changes: no lock outlives one call | `stale_writes.rs::no_lock_outlives_one_call` | Proven |
| File changes: every spelling of a file (`~/…` included) is one lock | `stale_writes.rs::a_home_spelling_of_a_file_waits_for_the_same_lock` | Proven |
| Memory protection (§14.6): an edit after another chat's write (two processes) is refused until reread, and a refused edit is no write in the chat's write set | `memory_protection.rs::an_edit_after_another_chats_write_is_refused_until_reread` | Proven (`edit_file::plan` without `file_versions.check`: failed) |
| Memory protection: an existing file never read can't be overwritten or edited, and the refusal is no write | `memory_protection.rs::an_existing_file_never_read_cant_be_overwritten` | Proven (`FileVersions::check` passing an unseen file: failed) |
| Memory protection: the check and the write are atomic across processes (a held `flock` makes the call wait, then it refuses the file made meanwhile) | `memory_protection.rs::the_check_and_write_are_atomic_across_processes` | Proven (`with_file_lock` ignoring `file_locks`: failed) |
| Memory protection: no lock outlives one call, and the kernel's write set is told only after the lock is let go | `memory_protection.rs::no_lock_outlives_one_call` | Proven (the `ctx.writes` report inside the lock's closure: failed) |
| Memory protection (P4): overlaps are shown as paths, never content: the desktop's shared paths, a `WriteOverlap` with who and how many, `mesh_list`'s paths only for a peer a rule lets send to the caller (never another's), tainting; a private chat's door, or one with cards off, says none | `memory_protection.rs::overlaps_are_shown_as_paths_never_content` | Proven (the rule filter dropped from the "editing:" line: failed; `WRITES_TAINT = false`: failed; `published_writes` without its cards/private check: failed) |
| Memory protection (P4): a peer's write set is cleaned on receipt: a path outside the folders it says it works in is dropped, and is no overlap | `memory_protection.rs::a_peers_write_set_is_cleaned_on_receipt` | Proven (`writes::received` keeping the set as sent: failed) |
| Memory protection (P1): granted chats sharing a folder default to their own worktrees | `memory_protection.rs::granted_chats_sharing_a_folder_default_to_worktrees` | Proven (`worktree_by_default` returning false: failed) |
| Images: any image taints | `images.rs::an_image_taints` | Proven |
| Images: sources, quarantined reader, sandboxed decoding | `images.rs::*` | Ignored: needs vision |
| `docs/TOOLS.md` is generated from the tools' code and current | `tools.rs::tools_md_matches_the_code` | Proven (`Glob::kind` changed to `Write`) |
| The built-in specs' bytes (names, order, descriptions, schemas) change only on purpose | `tools.rs::builtin_specs_match_the_golden` | Proven (a word added to `glob`'s description) |
| A session's tool list keeps the specs' order | `tools.rs::every_tool_list_keeps_the_specs_order` | Holds by construction (`tool_specs_for` filters) |
| A tool name is never reused; a removed tool has a tombstone | `tools.rs::a_removed_tool_keeps_its_name_and_tombstone` | Proven (two reverts: `memory` removed without a tombstone; a `glob` tombstone) |
| Who started a turn (the person, headless, a mesh wake, the radio, and a subagent of each of those four) changes approvals only as it did | `tools.rs::approvals_per_turn_match_the_golden` (`golden/approvals.txt`: every example call × 8 kinds of turn, clean and tainted, yolo off and on (left on in every kind of turn, so the mode alone gates it), with the sandbox on and every door open and with it off, through `run_tool` with each tool's effects stubbed) | Proven (`\|\| ctx.no_auto_approval` dropped from `once`; `ctx.mode.auto_approval() &&` dropped from the yolo check) |
| The floor (`approval_floor`, `docs/TOOLS.md`'s per-mode columns) is what `run_tool` decides | `tools.rs::the_floor_matches_todays_answers_per_mode` | Proven (`approval_floor` answering `Asks` for every-time calls) |
| Only `run_tool` runs a tool: no other non-test code calls a tool's `run`, `preview` or `approve_itself` | `tools.rs::only_run_tool_runs_a_tool` (the product's code under `crates/` parsed with syn, without `#[cfg(test)]` items and modules, `#[test]` functions and each crate's `tests/`: method calls of that name and arity, paths to one (`Tool::run`, `<T as Tool>::run`, an alias, a function pointer), a rename of the trait, `.run(…)`/`Type::run` in macro tokens; allowed: `run_tool`'s body, the blanket `impl DynTool for T`, and four named calls that run no tool, each of which must still match) | Proven (five kinds of call added to `table.rs`, each reported) |

## Notes

- **Found by the suite:** dangling symlinks. `innocent.sh -> .envrc`, where `.envrc` didn't exist yet, wasn't seen as protected, and `write_file` through `out.sh -> /elsewhere/x` wrote outside the project. Both are fixed in `paths::real_with_missing` (sandbox.md). Nested `.git` folders read-only for confined commands are proven by marv-tools' `an_approved_git_write_keeps_its_new_repository_and_confined_commands_see_nested_git_read_only`.
- **Fake homes live in `/var/tmp`** (`TempDir::outside_tmp`): the sandbox replaces `/tmp` with its own, so a fake home there would look hidden even without the home tmpfs.
- **The web guards use a local server** (`marv_guards::serve`) and a `WebFetch` that lets private addresses through: the guards never fetch the internet.
- **The Herdr door's guards** (`herdr.rs`) run a fake `herdr` (`marv_session::herdr_door::fake`, a shell script that logs every call and keeps its state in its own folder): never the user's Herdr, never `~/.config/herdr`. The `marv -p` one gives the binary a cleared environment with `HERDR_ENV` and a socket path that doesn't exist. Run the suite with Herdr's variables unset when you're inside Herdr.
- **The process door's guards** run `marv-guards-fixture` (`src/bin/`) as the relay, in place of `marv __relay`: a library's tests can't run the `marv` binary. The same fixture serves the forwarded test page from inside the sandbox and plays the parent that gets SIGKILLed. Each guard was proven by a scripted revert (one at a time, file restored after).
- **The doors' guards** (`github.rs`, `host.rs`): gh is a fake on PATH (`marv_tools::github::fake`, a shell script that logs every argv); never the real gh. The SDK guards that need it in the environment re-run themselves in a child process with it set (`marv_guards::in_child_with_env`: Rust can't change its own environment without `unsafe`). The `marv -p` guards run the binary `cargo test --workspace` builds (`marv_oracle::binary()`), against `marv_oracle`'s fake OpenRouter on port 0.
- **The lock-file guard** reads `Cargo.lock` at run time. To prove it, the test binary was built first, because cargo prunes an unused lock entry before it runs a test.
