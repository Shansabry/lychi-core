# Changelog

All notable changes to Lychi are documented here.

The format follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).
Pre-1.0, minor versions may contain breaking changes.

## [0.2.0](https://github.com/Shansabry/lychi-core/compare/v0.1.4...v0.2.0) (2026-08-18)


### Features

* ⌘K action panel, default-overlay config, guide + notes fixes ([cd8733c](https://github.com/Shansabry/lychi-core/commit/cd8733c4daad2b1ebb1906906e5100d6a07cf9bf))
* **agent:** live-streamed output, safer execution, and a smoother AI flow ([7f2997a](https://github.com/Shansabry/lychi-core/commit/7f2997aa787fc6cee9aa353bf518cf0b8e3a7164))
* **agent:** safer shell approval, resumable runs, and a leaner AI flow ([6706e9a](https://github.com/Shansabry/lychi-core/commit/6706e9a3fb7cc3c7e2a2b9df418d43dcc4ad51f7))
* **ai-presets:** the shipped defaults are read-only, and say so ([52a87ae](https://github.com/Shansabry/lychi-core/commit/52a87ae429afa0b951fa9da4cdcbafe093211b6e))
* **ai+hotkey:** inline tool artifacts (QR), hotkey autostart fix, viz polish ([c1bddbe](https://github.com/Shansabry/lychi-core/commit/c1bddbecf3b886da1cb0afd00d1a27e6e30c97a2))
* **ai:** add heuristic model potential meter ([10e5950](https://github.com/Shansabry/lychi-core/commit/10e5950c457c3642d17a87377aa0d8cb7298088d))
* **ai:** always-allow grants on approval prompts; deny feeds back ([a3a18c7](https://github.com/Shansabry/lychi-core/commit/a3a18c7ac1803295fd3a0f9a3ba276975ae65f91))
* **ai:** compact heavy conversation history in threshold batches ([e1033b6](https://github.com/Shansabry/lychi-core/commit/e1033b663aef0eff7888cf75152b08779068d38b))
* **ai:** declare every handler's grammar; the agent now sees 9 tools ([0fcc526](https://github.com/Shansabry/lychi-core/commit/0fcc5260bc88159dc2dfc647def0db15665ad67e))
* **ai:** give the agent a generated capability manifest ([687eb49](https://github.com/Shansabry/lychi-core/commit/687eb499e17175733a9caee590bd783e65a90cdd))
* **ai:** grammar-as-data core and grouped model-tool projection ([c1685a1](https://github.com/Shansabry/lychi-core/commit/c1685a1fd50447f4fedc5ea0c6670631afd53d48))
* **ai:** Phase 3 — AI presets (AI Commands) + delete old text-transform handlers ([06752ea](https://github.com/Shansabry/lychi-core/commit/06752ea896604bdae1dcfa32b709a074ce45dfd6))
* **ai:** Phase 4 history, live selection, weather fixes, and chat UX ([4599f6f](https://github.com/Shansabry/lychi-core/commit/4599f6f58a753a770e44184aed4f8280c887ff9c))
* **ai:** real web access — search and fetch tools for the agent ([c21c50d](https://github.com/Shansabry/lychi-core/commit/c21c50de19b84807e109cee4b849a4a8958f8dba))
* **ai:** rewrite AI into a streaming tool-calling agent ([a5bd41f](https://github.com/Shansabry/lychi-core/commit/a5bd41f0d68c489b4dc061a441de31156dc0ddbb))
* **ai:** run AI on the text selected in any application ([d5da9e5](https://github.com/Shansabry/lychi-core/commit/d5da9e5416f98d8d8e4e7c15009d02d559ad4b60))
* **ai:** show rate-limit retries live instead of a silent stall ([6e3e235](https://github.com/Shansabry/lychi-core/commit/6e3e235484ef41d47c3f210afd74721a6a12d7d3))
* **ai:** sticky tool selection and a find_tool discovery pseudo-tool ([91d56b7](https://github.com/Shansabry/lychi-core/commit/91d56b7bfd6adba975ef2577660cdf23582005e0))
* **ai:** the agent can see — screenshots flow back as images ([68c7c20](https://github.com/Shansabry/lychi-core/commit/68c7c2037354741370e8d892089ca09698c1edc7))
* **ai:** typed args for the bounded system tools ([bb2f48d](https://github.com/Shansabry/lychi-core/commit/bb2f48d297ff9200adfd92b20f660a1dbd8eb82f))
* **ai:** typed tool schemas; usage and context now reach the model ([c3eb462](https://github.com/Shansabry/lychi-core/commit/c3eb46277372e5c384edeaea2a9a25b9a97192f3))
* **appearance:** a customization tab with opacity, corners, frost, and system theme ([0445e7d](https://github.com/Shansabry/lychi-core/commit/0445e7d671cef5d02452bc1c5300d16245566ddf))
* **backup:** make losing your data recoverable ([6a31f08](https://github.com/Shansabry/lychi-core/commit/6a31f08310ca45a945ccebbc7b7f21ebd5a847f8))
* branding updates ([16ef26f](https://github.com/Shansabry/lychi-core/commit/16ef26f8f653fc8c2802b95c72b58a699fe78178))
* bundled local AI (llama.cpp), grammar-constrained routing, media UI ([e1915eb](https://github.com/Shansabry/lychi-core/commit/e1915ebf7736561a65e10601d081c351ec748a87))
* **clipboard:** never record password-manager copies ([0ef4b17](https://github.com/Shansabry/lychi-core/commit/0ef4b17e49e63d6727a93cd7cc1bea48a398b91f))
* configurable keybinds ([5a5525e](https://github.com/Shansabry/lychi-core/commit/5a5525ef905903376077a8180c49d1a466805bc4))
* context awareness — Phase 3.1 through 3.1g ([8ef2b8d](https://github.com/Shansabry/lychi-core/commit/8ef2b8d9bfb4a118d383b766e7af7e8901c4d914))
* **context:** ambient-state block formatter for agent turns ([3e72ee6](https://github.com/Shansabry/lychi-core/commit/3e72ee6d10ad47761c81846a0623b19885716c74))
* **context:** one session detector, capability probes, and `lychi doctor` ([9634d41](https://github.com/Shansabry/lychi-core/commit/9634d41d86fda7b387461bea1e5c6f90c1dacac6))
* **db:** versioned row envelope so a schema change cannot empty lists ([7153271](https://github.com/Shansabry/lychi-core/commit/7153271f3f981819235768cd4b69cedf234fae62))
* file preview, adaptive layout, context awareness fixes ([4e75f45](https://github.com/Shansabry/lychi-core/commit/4e75f457dbb9161c284fc42421071d06c66caf6e))
* **file-search:** index useful dotfiles with a structural bulk prune ([3bce5c3](https://github.com/Shansabry/lychi-core/commit/3bce5c30a1d9cf0dc89801e481694e8c4dd60626))
* **files+ai:** file-aware attachments, doc/vision AI, and actionable errors ([9940678](https://github.com/Shansabry/lychi-core/commit/99406784736e77b7801f11bf27b775550daab2d3))
* **filestore:** crash-recoverable JSONL + snapshot file stores ([cd4d9b0](https://github.com/Shansabry/lychi-core/commit/cd4d9b0e1f513e3d0dadaad551f0c7b64c47c040))
* filesystem fuzzy search updates ([b80a0d8](https://github.com/Shansabry/lychi-core/commit/b80a0d899a5d7ebf926021119b162c67cd9d88d8))
* filesystem updates ([4a1311f](https://github.com/Shansabry/lychi-core/commit/4a1311f6b6b4f9d8c5b8bfdc24b515b37d439328))
* GNOME hotkey fix, richer SSH host discovery, album art, and FE cleanup ([65b5d1e](https://github.com/Shansabry/lychi-core/commit/65b5d1e0aa268d53f08c92b41779846496ea438c))
* **hotkey:** register the global shortcut in the desktop's own settings ([f6f3fb4](https://github.com/Shansabry/lychi-core/commit/f6f3fb4ed70746fa44369c30eba357ba3a927140))
* **input:** fold text pastes into an atomic [copied text] token ([532f887](https://github.com/Shansabry/lychi-core/commit/532f887270e11884c83fcec91a47835754b02f3b))
* **media:** shuffle, repeat, volume/mute, time-skip, and a shared waveform ([9778d5e](https://github.com/Shansabry/lychi-core/commit/9778d5e314e7812cbaa804b386346d64537afa2c))
* more searches updates ([d549ad2](https://github.com/Shansabry/lychi-core/commit/d549ad217fe5f0e6358f63feaf420dc6b2be32b4))
* **onboarding:** give every first run something to say ([b8cc4d1](https://github.com/Shansabry/lychi-core/commit/b8cc4d150cd1dc32c28f4bfafadb1add2a1171b3))
* performance optimization ([71f45ca](https://github.com/Shansabry/lychi-core/commit/71f45ca36c70dd1379f07cb13a33d87df1b642c9))
* quicklinks, theme tokens, font picker, and honest suggestions ([397e5d9](https://github.com/Shansabry/lychi-core/commit/397e5d935623b7c4a41dee2af599626c7ce2cebe))
* redb implementation and preformance improvements ([507e66c](https://github.com/Shansabry/lychi-core/commit/507e66c641ab3028cb88ebb085fe68315b265559))
* routing strategy optimization ([7993659](https://github.com/Shansabry/lychi-core/commit/79936592993d8db0a90b5e161082da7b4d02233f))
* Script Commands + theming engine with WCAG-safe accents ([97628f1](https://github.com/Shansabry/lychi-core/commit/97628f1ecc962f74fb42fa56bdd22dae2391c9aa))
* **settings:** add a Plugins teaser tab ([e895f96](https://github.com/Shansabry/lychi-core/commit/e895f96d0243053773cfbbe1a55ee8c30eba8563))
* **settings:** auto-detect window strategy, enforce clipboard privacy, version config ([bfd7af9](https://github.com/Shansabry/lychi-core/commit/bfd7af9438f65ad8e368df2bfe6b2d4c6973b7c4))
* **settings:** font picker with per-row previews, custom kind dropdown ([6a087c2](https://github.com/Shansabry/lychi-core/commit/6a087c2ae62ddaf5f4acb7b32cf9d78852860fb9))
* **settings:** warn on a low max-tokens; unify the shared AI fields ([e967c71](https://github.com/Shansabry/lychi-core/commit/e967c71ed8a6b21451788694edaabd2b5d730d59))
* **setup:** in-app Setup tab replaces manual post-install steps ([4fd30bd](https://github.com/Shansabry/lychi-core/commit/4fd30bda25fcfbf0d13ae9f89dccecd9880a77d8))
* **startup:** show a dialog when a second instance cannot open the database ([25acd7c](https://github.com/Shansabry/lychi-core/commit/25acd7caca08bca04bbd27083f9d38e925a14d69))
* ui improvements ([a3c3928](https://github.com/Shansabry/lychi-core/commit/a3c3928b3655a7ce44502dce8ef4a7e2a8696629))
* unified AI switcher, @ file references, dynamic guide, placeholder rewrite ([360ac69](https://github.com/Shansabry/lychi-core/commit/360ac6906290105e4bba88b5befb073dc0d64bce))
* using icon packs ([b8edffc](https://github.com/Shansabry/lychi-core/commit/b8edffc43f3bd4154cf966aa131ffa3570c352ba))
* **ux:** natural-language suggestions, escape-hatch rows, consistent shortcuts ([eca964c](https://github.com/Shansabry/lychi-core/commit/eca964c35cfc877e9cd0e9f101d2a95e7fcb6175))
* weather report handling ([e7c25a7](https://github.com/Shansabry/lychi-core/commit/e7c25a7588c75b0555951f0a2810fa8be9acf543))
* window architecture and file preview ([d9c51b8](https://github.com/Shansabry/lychi-core/commit/d9c51b82242054724d8e2dab134a3013fe01b039))
* window strategy updates and optimizations ([e49159d](https://github.com/Shansabry/lychi-core/commit/e49159dd3ffa2795c4a02b0240735dcfe5ff9095))
* **zero-state:** pins + a sectioned composer for the empty prompt ([d5d0b8e](https://github.com/Shansabry/lychi-core/commit/d5d0b8eb153b85d028ebabac67ea4908acc7c045))
* **zero-state:** show the apps you use, not the text you typed ([215dede](https://github.com/Shansabry/lychi-core/commit/215dedec0668006fdbf8d8327c9d89cb4d752992))


### Bug Fixes

* **agent:** a stream error ends the turn, not the conversation ([39fe966](https://github.com/Shansabry/lychi-core/commit/39fe9664a1fe050c5fa176d223e94ef880c3386c))
* **agent:** an approval no longer drops the turn's sibling tool calls ([3ac4da4](https://github.com/Shansabry/lychi-core/commit/3ac4da4d81218c089f21b72dbc8f1e3ab4e80fe6))
* **agent:** don't run an identical tool call twice in one turn ([c0fe687](https://github.com/Shansabry/lychi-core/commit/c0fe687c7f3fb935477174b7aaa0ebd5aa8b31fc))
* **ai-history:** a corrupt old row no longer drops the new conversation ([105a1f9](https://github.com/Shansabry/lychi-core/commit/105a1f94300ab79234644cbdaa42eb77b4905c9c))
* **ai:** agent tool mutations refresh the notes panel ([33d4d6a](https://github.com/Shansabry/lychi-core/commit/33d4d6aa16c0cab2e14b4c5b5d6fe4603b7ad78d))
* **ai:** Always allow on a consent prompt grants the consent itself ([f2986d0](https://github.com/Shansabry/lychi-core/commit/f2986d0ffb3f6adfce631fc94d1f9aa317603f27))
* **ai:** an empty model turn retries once, then errors — never silence ([3cf8c2a](https://github.com/Shansabry/lychi-core/commit/3cf8c2a6b8425fa72fda3d49d179f143452934aa))
* **ai:** collapse TPM budget check into a let-chain (clippy) ([#18](https://github.com/Shansabry/lychi-core/issues/18)) ([12e6b6b](https://github.com/Shansabry/lychi-core/commit/12e6b6ba8d407fc0fe697be0c656d88b7cd0ff2d))
* **ai:** never answer changeable facts from memory — search first ([5510226](https://github.com/Shansabry/lychi-core/commit/5510226eaf9946508065d707b4b4f988b949fa7f))
* **ai:** precise word-level tool selection; trivia costs core only ([5c3fb9d](https://github.com/Shansabry/lychi-core/commit/5c3fb9d6bcc92185b8bfe957494211e8d26cc01a))
* **ai:** ration the tool payload to provider budgets; classify TPM 413s ([f9719e8](https://github.com/Shansabry/lychi-core/commit/f9719e8d17e671fa8e9350da1ab5fe9caffac13b))
* **ai:** re-select the agent's tools per turn, not once per task ([487db8a](https://github.com/Shansabry/lychi-core/commit/487db8adfb26561189ef351a76979200c543c883))
* **ai:** recover mangled gpt-oss tool calls instead of dying on them ([8f02624](https://github.com/Shansabry/lychi-core/commit/8f0262407b019f6e9a5d345051bf98aedb5997fe))
* **ai:** refuse to build a BYO provider with no model selected ([c9e4cf8](https://github.com/Shansabry/lychi-core/commit/c9e4cf874e9fd42479c7c68fb591a2e27a8c4ba9))
* **ai:** replay tool calls in their original shape — no phantom interface ([adc1022](https://github.com/Shansabry/lychi-core/commit/adc1022080df34181075ed200ebc2c39cc5f02e2))
* **ai:** report token usage for OpenAI-compatible providers ([4614a43](https://github.com/Shansabry/lychi-core/commit/4614a430784345929803961f264fbc21494efb16))
* **ai:** resolve `@`-referenced documents whose names contain spaces ([cb6090d](https://github.com/Shansabry/lychi-core/commit/cb6090dfe5d30566ef3ef05e0549f0927f5c5932))
* **ai:** retry a mid-stream provider rejection once before erroring ([6a22165](https://github.com/Shansabry/lychi-core/commit/6a22165ea06c5db6cea278e037e86038f54dd41c))
* **ai:** route AI commands to their preset, not the agent fallback ([207deb5](https://github.com/Shansabry/lychi-core/commit/207deb5001302b19313b2abcfab042caeeee4662))
* **ai:** stop duplicating built-in presets; move Agent safety to the AI page ([4f8d908](https://github.com/Shansabry/lychi-core/commit/4f8d908e2accf7b242c05fff2062950d182d4c53))
* **ai:** the ambient context block must not rank tools ([33675fe](https://github.com/Shansabry/lychi-core/commit/33675fea16c87b99ea4c5326e6365aecaf5395aa))
* **ai:** wrap wide markdown tables and gate the answer action on real artifacts ([a907a6f](https://github.com/Shansabry/lychi-core/commit/a907a6f39212d55dba154b4c183329f2b23a43e2))
* **audit:** B2, E3, F2, F4, F5 — say what is true, rank what is matched ([a0ddd0b](https://github.com/Shansabry/lychi-core/commit/a0ddd0b98e5774ca4246579240ad181ecc3da343))
* **backup:** capture the frecency DB and file stores, not just lychi.redb ([53986b3](https://github.com/Shansabry/lychi-core/commit/53986b3079aaa5801f889037bc61dc95c4f7ef28))
* **ci:** ask the WM for focus instead of assuming it grants it ([369a067](https://github.com/Shansabry/lychi-core/commit/369a067b9f4663a087e5bbbbafe46227177b9902))
* **ci:** pin least-privilege workflow token permissions ([#19](https://github.com/Shansabry/lychi-core/issues/19)) ([4f0009b](https://github.com/Shansabry/lychi-core/commit/4f0009b814b00f4fe9911d3c8886a6e1a0834a62))
* **ci:** release-please node strategy for workspace-inherited versions ([#23](https://github.com/Shansabry/lychi-core/issues/23)) ([6a38ee1](https://github.com/Shansabry/lychi-core/commit/6a38ee1e4e6ae9c4663b0c3a718189a57ac765ee))
* **ci:** satisfy the shellcheck actionlint actually runs ([1f375a9](https://github.com/Shansabry/lychi-core/commit/1f375a97b21a99289a29ad69af2194fc9fc99732))
* **ci:** the local-ai feature compiles in CI again — and stays compiled ([ad041a8](https://github.com/Shansabry/lychi-core/commit/ad041a89a48504dde1e4cfd9bf4e1b3814a7061a))
* **ci:** unbreak the workflow, and make it impossible to break it this way again ([332f8c9](https://github.com/Shansabry/lychi-core/commit/332f8c9b713bce8cf04bb497ef2602532f1aa2b5))
* **ci:** wait for the condition, not for a guessed number of seconds ([91bd488](https://github.com/Shansabry/lychi-core/commit/91bd4889b8af8a7c76d8005722c1514f172ef953))
* **cli:** don't panic when --help output is piped ([12324b8](https://github.com/Shansabry/lychi-core/commit/12324b8218463676a44f40db1d31bee09c1fe611))
* **cli:** handle every verb in the AppImage, add detached `start` ([eb207de](https://github.com/Shansabry/lychi-core/commit/eb207dead2ec1028e397f2838643095e7ee58d16))
* **clipboard:** event-driven capture on Wayland; stop the idle burn ([a7bb684](https://github.com/Shansabry/lychi-core/commit/a7bb684a7369d4b5c9367be626a31b3e1c408183))
* **confirm:** advertise one approve key, keep both working ([8c0e3fb](https://github.com/Shansabry/lychi-core/commit/8c0e3fb0d9647e2e3f5a5e6b253780045e4df2ab))
* **consent:** the confirmation carries its typed feature key to the FE ([479d371](https://github.com/Shansabry/lychi-core/commit/479d371fff66d8ddd24e04f4393fca281d488465))
* **context:** stop long-running resource leaks in caches, probes, and watchers ([3ad138c](https://github.com/Shansabry/lychi-core/commit/3ad138c01591b1fcc09b076ff4873028a107800a))
* **context:** the window map never waits on KWin D-Bus again ([4a97c8b](https://github.com/Shansabry/lychi-core/commit/4a97c8bbf06121895a7c28863c36607b9afea30c))
* **core:** bound corpus rebuilds, Sway abort, notify and terminal launch ([068c1d1](https://github.com/Shansabry/lychi-core/commit/068c1d1d9ae2996c7e55cb852daf1c67563eb1fe))
* data-safety, correctness, and streaming/UI hardening ([a5422c1](https://github.com/Shansabry/lychi-core/commit/a5422c12d730dee1b2f833c615b43111e4ec38d8))
* **db:** a corrupt row costs you that row, not the whole feature ([75d4f01](https://github.com/Shansabry/lychi-core/commit/75d4f01d1b774186b3f3f0507f0d326aa68e87af))
* **db:** a locked database is another instance, not a corrupt file ([9b26f67](https://github.com/Shansabry/lychi-core/commit/9b26f670318109de7d6ad56f0165be008603914e))
* **db:** recover rows stranded between schema generations ([a951bf7](https://github.com/Shansabry/lychi-core/commit/a951bf734fec6dbc6c53d229d3b33b60969b4725))
* **desktop-apps,text,config:** read the machine the user actually has ([84d65d3](https://github.com/Shansabry/lychi-core/commit/84d65d3c52978054f5667746519d35c9c784a173))
* **dismiss:** don't self-close on GNOME's spurious focus-out ([4109302](https://github.com/Shansabry/lychi-core/commit/4109302c3dd2767905f89071d87c2d09d6ac662c))
* **events:** async emitters hop to a blocking thread, and the rule is now enforced ([b73949f](https://github.com/Shansabry/lychi-core/commit/b73949fde52d2b89d191d76c191680d645da8a9d))
* **executor:** snapshot-and-release — the lock is never held across a handler again ([bb3394e](https://github.com/Shansabry/lychi-core/commit/bb3394e7e30607c008908a4c9d7378da592d3351))
* **file-search:** break the Arc cycle that kept evicted corpora from deallocating ([ad759d9](https://github.com/Shansabry/lychi-core/commit/ad759d969c725aa965f16f095f7bbd846891c0e1))
* **file-search:** drill into folders in the @ browser with Tab and arrow ([3d27d02](https://github.com/Shansabry/lychi-core/commit/3d27d0293cdd47fdb5cf4cd8db730c44319287d5))
* **file-search:** guarantee the terminal batch on a cold scope ([051cda8](https://github.com/Shansabry/lychi-core/commit/051cda883735d3f8e57f928d71dc8b3896306aad))
* **filestore,logging:** owner-only store files + quieten noisy log targets ([a202651](https://github.com/Shansabry/lychi-core/commit/a20265189a1c74fc16fae4b68d25920e8208327e))
* **frecency:** prefix range scans on the keystroke path, and dead learning rows now die ([7fae3fd](https://github.com/Shansabry/lychi-core/commit/7fae3fd0ffed3c343be3fae28fd5e532ba6913e2))
* harden temp-file, idle-work, log-growth, and doc-drift edges ([fd18bcf](https://github.com/Shansabry/lychi-core/commit/fd18bcfbba5e50c635c3d3c1da2d099a2661cb9e))
* **hotkey:** a portal restart silences the stream, it does not end it ([9e57b50](https://github.com/Shansabry/lychi-core/commit/9e57b509fabad7cc6936d560d4934aafe438e030))
* **hotkey:** bind the portal shortcut every launch, not only when absent ([35ec34f](https://github.com/Shansabry/lychi-core/commit/35ec34f794e23559f317511d17bcfca4f6753788))
* **hotkey:** move the desktop binding when the hotkey changes in Settings ([ec75f9b](https://github.com/Shansabry/lychi-core/commit/ec75f9b5a880e7b0cc009b8ecb8232a5d11cadc4))
* **hotkey:** re-register when the portal session dies; tell the truth meanwhile ([67c97c7](https://github.com/Shansabry/lychi-core/commit/67c97c7b8fd41c761e19c6ae6a4b2af4b11fa532))
* **hotkey:** register the portal app-id on the connection that opens the session ([5856a01](https://github.com/Shansabry/lychi-core/commit/5856a016029bf6852fc6ca869e0da095bec6e36c))
* **hotkey:** stop reporting an unproven X11 grab as reliable ([7d836b4](https://github.com/Shansabry/lychi-core/commit/7d836b48c0ee741172df4c77fd4a0cf1109ba970))
* **hotkey:** survive reboot/autostart on Wayland via app-id self-registration ([bd389ad](https://github.com/Shansabry/lychi-core/commit/bd389ad4fb378f0a9fc535c7fbaed56ae226580c))
* **icons:** re-resolve cached icon paths that no longer exist ([64c74cd](https://github.com/Shansabry/lychi-core/commit/64c74cd658d8632959ec320354888ca8e204a639))
* **launcher:** own the window state and take focus from the Wayland protocol ([192d49e](https://github.com/Shansabry/lychi-core/commit/192d49e35c4b6307a22a906f55f86f5fe11096ff))
* **linux:** survive hosts without GStreamer, stop blacking out on GNOME ([56ea95d](https://github.com/Shansabry/lychi-core/commit/56ea95d283478de5053154f5fe7d4b55ca916600))
* **local-ai:** poll the cancel token per token so a stopped turn stops ([16dd347](https://github.com/Shansabry/lychi-core/commit/16dd34763883b9c06809679f5560b8fe9b11ef21))
* **logging:** cap log retention at 7 days ([49dfef2](https://github.com/Shansabry/lychi-core/commit/49dfef2232e2eeb9039808a4913504cca6b7def5))
* **logging:** keep what the user typed out of the shareable log file ([026c31a](https://github.com/Shansabry/lychi-core/commit/026c31a1e774e32f5778053a179883f1fcf8b01b))
* **platform:** derive the kde fact from the compositor decider, not a private env parse ([3a7f45e](https://github.com/Shansabry/lychi-core/commit/3a7f45e4d77c34466e603a26be634dd1d01a46e9))
* **platform:** use one Wayland-detection rule for the window strategy ([9883447](https://github.com/Shansabry/lychi-core/commit/9883447e2aa3da048f6ba01d088b2d71febc1719))
* **providers:** a silent connection can no longer park an AI turn forever ([474d75d](https://github.com/Shansabry/lychi-core/commit/474d75dc44d72f13c149634429a2278762c6c24d))
* **registry:** surface trigger-prefix collisions instead of silent steal ([1690ecd](https://github.com/Shansabry/lychi-core/commit/1690ecdbf9d8f09764946a5de51ec62a18e122b3))
* **release:** purge stale bundle outputs from the cache; v0.1.3 ([#25](https://github.com/Shansabry/lychi-core/issues/25)) ([cb919d1](https://github.com/Shansabry/lychi-core/commit/cb919d1272c787941dbf9d021a4fee407f9cf1d3))
* **release:** stop hardcoding the AppImage filename; v0.1.4 ([#27](https://github.com/Shansabry/lychi-core/issues/27)) ([42e09fe](https://github.com/Shansabry/lychi-core/commit/42e09fe201b1f06534634936f481b14216389ed2))
* **routing:** classify_string applies the same app-identity gate as resolve ([609eab9](https://github.com/Shansabry/lychi-core/commit/609eab92e8b14308ac933cc73887c6f27b24f8a8))
* **rules:** the handler declares consent; the engine stops re-parsing args ([fe3d8d2](https://github.com/Shansabry/lychi-core/commit/fe3d8d2bd15890de413f30f99d1488c29c5970c0))
* **settings:** frontend max-tokens fallback matches the real default ([3d16248](https://github.com/Shansabry/lychi-core/commit/3d162489dd54a79a2bf672fc1843ab8d3fe4176c))
* **settings:** keep sliders in sync after save; widen the sidebar ([30fe3f9](https://github.com/Shansabry/lychi-core/commit/30fe3f9394209d8f52d49e6ef191a6c814a242f9))
* **shell-exec:** one capture core, and a timeout that actually kills ([ba42ab5](https://github.com/Shansabry/lychi-core/commit/ba42ab546425a8999033fcfbff1eb2ca13887799))
* **state:** make the config/executor deadlock cycle unrepresentable ([27934ba](https://github.com/Shansabry/lychi-core/commit/27934ba79efd7e0e16d58e4df51d472df4b66f68))
* **submit:** act on what was on screen when Enter was pressed ([61a1df2](https://github.com/Shansabry/lychi-core/commit/61a1df25cefd0ad0e350a195f7732b1018b374ec))
* **suggestions:** a label the user is typing toward counts for Enter ([a98d8f2](https://github.com/Shansabry/lychi-core/commit/a98d8f24910ce5288719f23bac9b38f5b4789baf))
* **suggestions:** a natural-language query stops matching every app ([d190ea0](https://github.com/Shansabry/lychi-core/commit/d190ea0b43138cfeb1f51e5d959fb0a5a0f5bdaa))
* **suggestions:** auto-select an app matched by its acronym ([52535fb](https://github.com/Shansabry/lychi-core/commit/52535fb0060f28dc5603854315f35e1e276676ee))
* **suggestions:** key dedupe and latching on the command, never the label ([eb6e462](https://github.com/Shansabry/lychi-core/commit/eb6e4629db38ba98eebfadfca8e50629ff04055c))
* **suggestions:** send the defaultability verdict instead of its inputs ([412b148](https://github.com/Shansabry/lychi-core/commit/412b148d20985abf6f87efc84865597679651026))
* **suggestions:** simpler "Did you mean" and cleaner fallback rows ([4411a21](https://github.com/Shansabry/lychi-core/commit/4411a21bb84a752dbeaf44cfc7c3e0b586d837d5))
* **suggest:** questions never get repo-run rows, even with binary heads ([b9d4c2a](https://github.com/Shansabry/lychi-core/commit/b9d4c2aabda1ea27e75d8651a2b8cbf1ca843a4d))
* **summon:** never silently drop freshly gathered context ([d07f96b](https://github.com/Shansabry/lychi-core/commit/d07f96bb98605c983269a15f3154f7a7423a7fac))
* terminal routing optimization and fixes ([8f71e94](https://github.com/Shansabry/lychi-core/commit/8f71e94edaf2c03aa8c10d0d38e192355f6b1d5e))
* **terminal:** run interactive commands in a terminal, in a clean environment ([8a94594](https://github.com/Shansabry/lychi-core/commit/8a94594f4521ceb771ce47c202e82ad7b4534339))
* **timer:** count suspend time — a timer is a wall-clock promise ([22e678d](https://github.com/Shansabry/lychi-core/commit/22e678dc0213fb3a6b7d8ef30c7528566b358c48))
* **ui,system,bookmarks:** make failures visible and phrase tables derivable ([ccfea4c](https://github.com/Shansabry/lychi-core/commit/ccfea4c65684014b78ebdccf903329955a2f7e01))
* **updater:** the plugin needs its config section to exist, even when unused ([5066b2a](https://github.com/Shansabry/lychi-core/commit/5066b2aa112ce811b45396d3b55d8d0020eb7a05))
* **webview:** allow ipc: in the CSP, and log frontend errors ([35d088a](https://github.com/Shansabry/lychi-core/commit/35d088adbc98644aae8c3e80c15ec0dc65d75ff9))
* **webview:** recover from a dead WebProcess instead of going blank ([ae7f15a](https://github.com/Shansabry/lychi-core/commit/ae7f15a2415a1d0acf9bc5af59ccaad8a2752d43))
* **window:** don't dismiss on focus loss before the user has interacted ([db2a29a](https://github.com/Shansabry/lychi-core/commit/db2a29a0273f9272677903c776e5bcd256f6d213))
* **window:** eliminate the re-summon flash of stale content ([91f3605](https://github.com/Shansabry/lychi-core/commit/91f3605a8e2379efaf27e26537370aa1e68c76fb))
* **zero-state:** workspace memory no longer duplicates an app row ([eca6fc2](https://github.com/Shansabry/lychi-core/commit/eca6fc2c8ed894e3947eb3b6cc2d3409e43765f3))


### Performance Improvements

* **agent:** send a query-relevant tool subset, core always kept ([c14ee44](https://github.com/Shansabry/lychi-core/commit/c14ee4416f90da0abb47995c16ff33a1e5d14b91))
* **ai-history:** list without parsing bodies; skip corrupt rows ([9370a89](https://github.com/Shansabry/lychi-core/commit/9370a89edfbdb09995699a830186f98f7450379a))
* **ai:** cut agent input tokens with per-query tool selection ([baa26df](https://github.com/Shansabry/lychi-core/commit/baa26dfcd09397fd43e5e180f3e9398bd46b039f))
* **ai:** run read-only sibling tool calls concurrently within a turn ([b2b12ce](https://github.com/Shansabry/lychi-core/commit/b2b12ce4f7833010b2bb6efcb764fccb72e6a3df))
* **completions:** stop spawning subprocesses and rescanning per keystroke ([22224f9](https://github.com/Shansabry/lychi-core/commit/22224f946ca458290cf49b84885cf6fedaedf674))
* **file-search:** evict path corpora for scopes nobody is searching ([61793a2](https://github.com/Shansabry/lychi-core/commit/61793a2bef068257343763e13d36eb928459a110))
* **file-search:** hold path text in one arena instead of 3 Strings per path ([d5d58a3](https://github.com/Shansabry/lychi-core/commit/d5d58a3580133f3d6eec9dd4cccf4f68f55cdc2a))
* **file-search:** release the search matcher when searching goes idle ([e5febfa](https://github.com/Shansabry/lychi-core/commit/e5febfa068333412128f562122149f2c837fc5ea))
* **file-search:** stat lazily and rewrite the recency curve ([4107e56](https://github.com/Shansabry/lychi-core/commit/4107e566c662da56406865d5b0fdd47ab19395da))
* **history:** delete entries instead of tombstoning them forever ([70e5829](https://github.com/Shansabry/lychi-core/commit/70e5829cd566da0cd35f119e195e9184bce86df2))

## [Unreleased]

### Added

- **Launcher core** — fuzzy app launching via XDG `.desktop` discovery, fuzzy
  file search with frecency ranking, shell execution, web and YouTube search
  with user-definable search engines ("bangs"), math/unit/currency evaluation,
  and project opening.
- **~45 built-in commands**, including clipboard history, snippets, aliases,
  notes and todos, timers and reminders, screenshots, systemd service control,
  package management (dnf/apt/pacman/zypper/flatpak), window switching, SSH
  hosts, browser bookmarks, developer utilities (base64/hash/urlencode/epoch/
  json/text-case), QR codes, emoji and Unicode search, colour conversion,
  dictionary, weather, and system info.
- **Quicklinks** — parameterized user-defined commands that expand to a URL,
  shell command, path, or another Lychi command, with escaping applied per
  destination.
- **Script Commands** — any executable in `~/.config/lychi/scripts/` becomes a
  named command, hot-reloaded on change.
- **AI (optional, off by default)** — four modes: disabled, BYO key, Ollama, or
  a bundled local model (llama.cpp, CPU-only). BYO supports Anthropic, OpenAI,
  Groq, Grok, Gemini, OpenRouter, or any custom endpoint; the model is always
  user-typed rather than picked from a baked-in list. Includes a streaming
  tool-calling agent, user-defined AI Commands, chat history, file attachments
  with document and vision support, and running AI over text selected in any
  application.
- **Context awareness** — commands resolve against the focused terminal or IDE
  (working directory, git repository, project), including multi-repository
  workspaces.
- **Ctrl+K action panel** — per-result actions, with fully configurable
  keybindings.
- **Theming** — WCAG-safe accent generation and a font picker that previews
  each installed typeface in itself.
- **Desktop integration** — wlr-layer-shell on wlroots compositors, toplevel
  windows on KDE and GNOME, X11 fallback with a compact mode for
  non-composited sessions; XDG GlobalShortcuts portal for the global hotkey;
  MPRIS media control; tray icon and autostart.
- **CLI** — `lychi start`, `--toggle`, `--screenshot [area|window]`,
  `--ai [preset]`, all over a Unix socket so they're cheap to bind to desktop
  shortcuts.

### Security

- **Central permission deciders** — every execution passes through one decider
  per surface (`rules/shell.rs`, `path.rs`, `uri.rs`), closing two paths where
  script commands and AI-generated plans could reach a shell without the gate.
  API keys are stored in the system keyring, never on disk in plain text.
  BYO endpoints are required to be HTTPS unless they're loopback.

### Fixed

- **GNOME Wayland: blank window on any input.** WebKitGTK initialises a
  GStreamer pipeline in every WebProcess regardless of page content; on hosts
  without `gst-plugins-base` the process died and left the UI blank while the
  app kept running. Lychi has no media elements, so the media stack is now
  switched off entirely rather than the codecs bundled.
- **GNOME Wayland: launcher rendered as an opaque full-screen panel.** Mutter
  paints a black backdrop behind fullscreen windows, which defeated the
  transparent monitor-covering surface the launcher is centred on. Fullscreen
  is no longer requested on Mutter-based desktops.
- **AppImage bundling** now follows an explicit keep-list rather than probing
  the build machine, which had made the artifact depend on which packages the
  builder happened to have installed.
- **CLI verbs** are handled by the AppImage itself, so `lychi --help` prints
  usage instead of launching a second window.
- Token usage is reported for OpenAI-compatible providers.

### Known limitations

- First-run guidance is limited to contextual hints (such as the Wayland hotkey
  banner); there is no guided onboarding.
- The window appears in the taskbar on KDE Wayland
  ([tauri#9829](https://github.com/tauri-apps/tauri/issues/9829)).
- AppImage is currently the only distribution channel.

[Unreleased]: https://github.com/Shansabry/lychi-core/commits/main
