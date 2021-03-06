# Cicada Completions

## Builtin Completions

Path completion is available out of box. In addition to this, cicada also
support completion for command:

- make
- ssh
- vox

### Completions on OS Envs

```
$ echo $LC_<TAB><TAB>
$LC_ALL  $LC_CTYPE
```

## Customize Completions

Cicada also supports simplifed customized completion using YAML file.
Put your completion files under `~/.cicada/completers/`. The completion files
look like this:

```
$ ls ~/.cicada/completers/
brew.yaml git.yaml  pip.yaml  vox.yaml

$ cat ~/.cicada/completers/brew.yaml
- doctor
- info
- install
- list
- search
- uninstall
- update
- upgrade

$ brew u<Tab><Tab>
uninstall  update  upgrade
```

Currently, cicada supports maximum 2 level completion:

```
$ cat ~/.cicada/completers/pip.yaml
- install:
    - -U
    - --upgrade
    - -r
    - --requirement
- download
- uninstall
- search:
    - --no-cache-dir
    - --timeout
- wheel

$ pip ins<Tab>
$ pip install

$ pip install --re<Tab>
$ pip install --requirement
```
