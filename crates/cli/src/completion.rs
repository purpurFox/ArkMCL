//! Shell completion scripts.

const BASH: &str = r#"# bash completion for arkmcl
# install: arkmcl completion bash > ~/.local/share/bash-completion/completions/arkmcl
_arkmcl_complete() {
    local cur prev cmd word
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"
    cmd=""
    for word in "${COMP_WORDS[@]:1}"; do
        case "$word" in
            account|instance|launch|resource|service|info|version|help|completion) cmd="$word"; break ;;
        esac
    done

    case "$prev" in
        --lang) COMPREPLY=( $(compgen -W "zh-CN en-US" -- "$cur") ); return ;;
        --api) COMPREPLY=( $(compgen -W "modrinth curseforge" -- "$cur") ); return ;;
        --filter-vanilla) COMPREPLY=( $(compgen -W "all stable snapshot foolsday legacy" -- "$cur") ); return ;;
        --filter-loader)
            COMPREPLY=( $(compgen -W "fabric forge neoforge purpur" -- "$cur") ); return ;;
        --loader)
            if [ "$cmd" = "instance" ]; then
                COMPREPLY=( $(compgen -W "fabric forge neoforge" -- "$cur") )
            else
                COMPREPLY=( $(compgen -W "fabric forge neoforge purpur" -- "$cur") )
            fi
            return ;;
        --type)
            if [ "$cmd" = "instance" ]; then
                COMPREPLY=( $(compgen -W "client server" -- "$cur") )
            elif [ "$cmd" = "resource" ]; then
                COMPREPLY=( $(compgen -W "worlds resourcepacks datapacks shaderpacks mods plugins modpacks" -- "$cur") )
            fi
            return ;;
        --list-type) COMPREPLY=( $(compgen -W "mod modpack resourcepack shader datapack plugin world" -- "$cur") ); return ;;
        --linking) COMPREPLY=( $(compgen -W "all none" -- "$cur") ); return ;;
        --create-defaults) COMPREPLY=( $(compgen -W "true false" -- "$cur") ); return ;;
        --jvm-args) COMPREPLY=( $(compgen -W "default none" -- "$cur") ); return ;;
        --lwjgl) COMPREPLY=( $(compgen -W "default" -- "$cur") ); return ;;
        --list-sort) COMPREPLY=( $(compgen -W "asc desc" -- "$cur") ); return ;;
        --list-sort-by)
            if [ "$cmd" = "resource" ]; then
                if [[ " ${COMP_WORDS[*]} " == *" --list-remote "* && " ${COMP_WORDS[*]} " == *" --filter-loader "* ]]; then
                    COMPREPLY=( $(compgen -W "create download name relevance" -- "$cur") )
                elif [[ " ${COMP_WORDS[*]} " == *" --list-remote "* ]]; then
                    COMPREPLY=( $(compgen -W "create launch alphabet" -- "$cur") )
                elif [[ " ${COMP_WORDS[*]} " == *" --search "* || " ${COMP_WORDS[*]} " == *" --list-search "* ]]; then
                    COMPREPLY=( $(compgen -W "create download name relevance" -- "$cur") )
                else
                    COMPREPLY=( $(compgen -W "create alphabet" -- "$cur") )
                fi
            else
                COMPREPLY=( $(compgen -W "create launch run alphabet" -- "$cur") )
            fi
            return ;;
        --group|--group-move) COMPREPLY=( $(compgen -W "$(arkmcl complete groups 2>/dev/null)" -- "$cur") ); return ;;
        --download-to-instance|--list-download-to-instance) COMPREPLY=( $(compgen -W "$(arkmcl complete instance-ids 2>/dev/null)" -- "$cur") ); return ;;
    esac

    if [ -z "$cmd" ]; then
        if [[ "$cur" == -* ]]; then
            COMPREPLY=( $(compgen -W "--help --json --lang --list-sort --list-sort-by --version --yes" -- "$cur") )
        else
            COMPREPLY=( $(compgen -W "account instance launch resource service info version help completion" -- "$cur") )
        fi
        return
    fi

    if [[ "$cur" == -* ]]; then
        case "$cmd" in
            account) COMPREPLY=( $(compgen -W "--add --default --json --lang --list --list-sort --list-sort-by --refresh --remove --server --yes" -- "$cur") ) ;;
            instance) COMPREPLY=( $(compgen -W "--account --create --create-defaults --default --group --group-move --import --info --json --lang --linking --list --list-sort --list-sort-by --loader --loader-version --lwjgl --memory --move --name --remove --type --version --yes" -- "$cur") ) ;;
            launch) COMPREPLY=( $(compgen -W "--account --json --lang --list --list-sort --list-sort-by --prepare --start --stop --terminal --yes" -- "$cur") ) ;;
            resource) COMPREPLY=( $(compgen -W "--api --autoremove --download --download-datapack --download-mod --download-modpack --download-plugin --download-resourcepack --download-shader --download-to-instance --download-version --download-world --filter-loader --filter-vanilla --filter-version --game-version --json --lang --list --list-download-to-instance --list-limit --list-remote --list-search --list-sort --list-sort-by --list-type --loader --lookup --search --type --yes" -- "$cur") ) ;;
            service) COMPREPLY=( $(compgen -W "--lang" -- "$cur") ) ;;
            completion) COMPREPLY=( $(compgen -W "bash zsh fish" -- "$cur") ) ;;
        esac
        return
    fi

    case "$cmd" in
        account) COMPREPLY=( $(compgen -W "$(arkmcl complete account-names 2>/dev/null)" -- "$cur") ) ;;
        instance|launch) COMPREPLY=( $(compgen -W "$(arkmcl complete instance-names 2>/dev/null)" -- "$cur") ) ;;
        service) COMPREPLY=( $(compgen -W "status stop" -- "$cur") ) ;;
        completion) COMPREPLY=( $(compgen -W "bash zsh fish" -- "$cur") ) ;;
    esac
}
complete -F _arkmcl_complete arkmcl
"#;

const ZSH: &str = r#"#compdef arkmcl
# install: arkmcl completion zsh > "${fpath[1]}/_arkmcl"
_arkmcl() {
    local -a commands
    local cmd="" word
    commands=(account instance launch resource service info version help completion)
    for word in ${words[2,-1]}; do
        case $word in
            account|instance|launch|resource|service|info|version|help|completion) cmd=$word; break ;;
        esac
    done

    local cur="${words[CURRENT]}" prev="${words[CURRENT-1]}"
    case "$prev" in
        --lang) compadd zh-CN en-US; return ;;
        --api) compadd modrinth curseforge; return ;;
        --filter-vanilla) compadd all stable snapshot foolsday legacy; return ;;
        --filter-loader) compadd fabric forge neoforge purpur; return ;;
        --loader)
            if [[ "$cmd" == instance ]]; then
                compadd fabric forge neoforge
            else
                compadd fabric forge neoforge purpur
            fi
            return ;;
        --type)
            if [[ "$cmd" == instance ]]; then
                compadd client server
            elif [[ "$cmd" == resource ]]; then
                compadd worlds resourcepacks datapacks shaderpacks mods plugins modpacks
            fi
            return ;;
        --list-type) compadd mod modpack resourcepack shader datapack plugin world; return ;;
        --linking) compadd all none; return ;;
        --create-defaults) compadd true false; return ;;
        --jvm-args) compadd default none; return ;;
        --lwjgl) compadd default; return ;;
        --list-sort) compadd asc desc; return ;;
        --list-sort-by)
            if [[ "$cmd" == resource ]]; then
                if [[ "${words[*]}" == *--list-remote* && "${words[*]}" == *--filter-loader* ]]; then
                    compadd create download name relevance
                elif [[ "${words[*]}" == *--list-remote* ]]; then
                    compadd create launch alphabet
                elif [[ "${words[*]}" == *--search* || "${words[*]}" == *--list-search* ]]; then
                    compadd create download name relevance
                else
                    compadd create alphabet
                fi
            else
                compadd create launch run alphabet
            fi
            return ;;
        --group|--group-move) compadd -- ${(f)"$(arkmcl complete groups 2>/dev/null)"}; return ;;
        --download-to-instance|--list-download-to-instance) compadd -- ${(f)"$(arkmcl complete instance-ids 2>/dev/null)"}; return ;;
    esac

    if [[ -z "$cmd" ]]; then
        if [[ "$cur" == -* ]]; then
            compadd -- --help --json --lang --list-sort --list-sort-by --version --yes
        else
            compadd -a commands
        fi
        return
    fi

    if [[ "$cur" == -* ]]; then
        case "$cmd" in
            account) compadd -- --add --default --json --lang --list --list-sort --list-sort-by --refresh --remove --server --yes ;;
            instance) compadd -- --account --create --create-defaults --default --group --group-move --import --info --json --lang --linking --list --list-sort --list-sort-by --loader --loader-version --lwjgl --memory --move --name --remove --type --version --yes ;;
            launch) compadd -- --account --json --lang --list --list-sort --list-sort-by --prepare --start --stop --terminal --yes ;;
            resource) compadd -- --api --autoremove --download --download-datapack --download-mod --download-modpack --download-plugin --download-resourcepack --download-shader --download-to-instance --download-version --download-world --filter-loader --filter-vanilla --filter-version --game-version --json --lang --list --list-download-to-instance --list-limit --list-remote --list-search --list-sort --list-sort-by --list-type --loader --lookup --search --type --yes ;;
            service) compadd -- --lang ;;
            completion) compadd bash zsh fish ;;
        esac
        return
    fi

    case "$cmd" in
        account) compadd -- ${(f)"$(arkmcl complete account-names 2>/dev/null)"} ;;
        instance|launch) compadd -- ${(f)"$(arkmcl complete instance-names 2>/dev/null)"} ;;
        service) compadd status stop ;;
        completion) compadd bash zsh fish ;;
    esac
}
_arkmcl "$@"
"#;

const FISH: &str = r"# fish completion for arkmcl
# install: arkmcl completion fish > ~/.config/fish/completions/arkmcl.fish
complete -c arkmcl -f

complete -c arkmcl -n '__fish_use_subcommand' -a account -d 'Manage accounts'
complete -c arkmcl -n '__fish_use_subcommand' -a instance -d 'Manage instances'
complete -c arkmcl -n '__fish_use_subcommand' -a launch -d 'Manage runs'
complete -c arkmcl -n '__fish_use_subcommand' -a resource -d 'Browse resources'
complete -c arkmcl -n '__fish_use_subcommand' -a service -d 'Service status and stop'
complete -c arkmcl -n '__fish_use_subcommand' -a info -d 'Runtime information'
complete -c arkmcl -n '__fish_use_subcommand' -a version -d 'Print version'
complete -c arkmcl -n '__fish_use_subcommand' -a help -d 'Print help'
complete -c arkmcl -n '__fish_use_subcommand' -a completion -d 'Shell completion'

complete -c arkmcl -n '__fish_use_subcommand' -l help -s h -d 'Print help'
complete -c arkmcl -n '__fish_use_subcommand' -l version -s V -d 'Print version'
complete -c arkmcl -l json -d 'JSON output'
complete -c arkmcl -l yes -d 'Skip confirmation'
complete -c arkmcl -l lang -r -f -a 'zh-CN en-US'

complete -c arkmcl -n '__fish_seen_subcommand_from account' -l list -d 'List accounts'
complete -c arkmcl -n '__fish_seen_subcommand_from account' -l add -d 'Add account'
complete -c arkmcl -n '__fish_seen_subcommand_from account' -l remove -d 'Remove accounts'
complete -c arkmcl -n '__fish_seen_subcommand_from account' -l default -d 'Set default account'
complete -c arkmcl -n '__fish_seen_subcommand_from account' -l refresh -d 'Refresh tokens'
complete -c arkmcl -n '__fish_seen_subcommand_from account' -l server -r -d 'External server'
complete -c arkmcl -n '__fish_seen_subcommand_from account' -l list-sort -x -a 'asc desc'
complete -c arkmcl -n '__fish_seen_subcommand_from account' -l list-sort-by -x -a 'create launch run alphabet'
complete -c arkmcl -n '__fish_seen_subcommand_from account' -a '(arkmcl complete account-names 2>/dev/null)'

complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l account -x -a '(arkmcl complete account-names 2>/dev/null)'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l create -d 'Create instance'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l create-defaults -x -a 'true false' -d 'Copy default config'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l default -d 'Set default instance'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l group -x -a '(arkmcl complete groups 2>/dev/null)' -d 'Group'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l group-move -x -a '(arkmcl complete groups 2>/dev/null)' -d 'Move group'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l import -r -d 'Import .mrpack'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l info -d 'Instance info'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l linking -x -a 'all none' -d 'Shared links'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l list -d 'List instances'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l list-sort -x -a 'asc desc'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l list-sort-by -x -a 'create launch run alphabet'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l loader -x -a 'fabric forge neoforge'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l loader-version -r -d 'Loader version'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l lwjgl -x -a 'default' -d 'LWJGL override'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l memory -r -d 'Memory limit (MB)'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l move -r -d 'Rename instance id'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l name -r -d 'Set name'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l remove -d 'Remove instances'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l type -x -a 'client server' -d 'Instance type'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l version -r -d 'Minecraft version'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -l jvm-args -x -a 'default none' -d 'JVM arguments'
complete -c arkmcl -n '__fish_seen_subcommand_from instance' -a '(arkmcl complete instance-names 2>/dev/null)'

complete -c arkmcl -n '__fish_seen_subcommand_from launch' -l account -x -a '(arkmcl complete account-names 2>/dev/null)'
complete -c arkmcl -n '__fish_seen_subcommand_from launch' -l list -d 'List running instances'
complete -c arkmcl -n '__fish_seen_subcommand_from launch' -l list-sort -x -a 'asc desc'
complete -c arkmcl -n '__fish_seen_subcommand_from launch' -l list-sort-by -x -a 'create launch run alphabet'
complete -c arkmcl -n '__fish_seen_subcommand_from launch' -l prepare -d 'Prepare resources'
complete -c arkmcl -n '__fish_seen_subcommand_from launch' -l start -d 'Start instance'
complete -c arkmcl -n '__fish_seen_subcommand_from launch' -l stop -d 'Force-terminate instances'
complete -c arkmcl -n '__fish_seen_subcommand_from launch' -l terminal -d 'Attach console'
complete -c arkmcl -n '__fish_seen_subcommand_from launch' -a '(arkmcl complete instance-names 2>/dev/null)'

complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l api -x -a 'modrinth curseforge'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l autoremove -d 'Remove unused resources'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l download -r -d 'Download project'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l download-datapack -r -d 'Download datapack'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l download-mod -r -d 'Download mod'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l download-modpack -r -d 'Download modpack'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l download-plugin -r -d 'Download plugin'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l download-resourcepack -r -d 'Download resourcepack'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l download-shader -r -d 'Download shader'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l download-to-instance -x -a '(arkmcl complete instance-ids 2>/dev/null)'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l download-version -r -d 'Download version'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l download-world -r -d 'Download world'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l filter-loader -x -a 'fabric forge neoforge purpur'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l filter-vanilla -x -a 'all stable snapshot foolsday legacy'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l filter-version -r -d 'Version pattern'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l game-version -r -d 'Game version'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l list -d 'List local resources'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l list-download-to-instance -x -a '(arkmcl complete instance-ids 2>/dev/null)'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l list-limit -r -d 'Result limit'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l list-remote -d 'Remote versions'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l list-search -r -d 'Search query'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l list-sort -x -a 'asc desc'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l list-sort-by -x -a 'create launch alphabet download name relevance'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l list-type -x -a 'mod modpack resourcepack shader datapack plugin world'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l loader -x -a 'fabric forge neoforge purpur'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l lookup -r -d 'Project versions'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l search -r -d 'Community search'
complete -c arkmcl -n '__fish_seen_subcommand_from resource' -l type -x -a 'worlds resourcepacks datapacks shaderpacks mods plugins modpacks'

complete -c arkmcl -n '__fish_seen_subcommand_from service' -a 'status stop'
complete -c arkmcl -n '__fish_seen_subcommand_from completion' -a 'bash zsh fish'
";

/// Return a completion script for the given shell.
#[must_use]
pub fn script(shell: &str) -> Option<&'static str> {
    match shell {
        "bash" => Some(BASH),
        "zsh" => Some(ZSH),
        "fish" => Some(FISH),
        _ => None,
    }
}
