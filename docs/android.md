# Serveur GameSync sur Android avec Termux

Cette procédure expérimentale utilise le téléphone comme serveur de sauvegardes, sans application Android graphique. Elle n'a pas encore été validée sur un téléphone réel. Garder une copie indépendante de la sauvegarde avant tout essai et commencer par un dossier fictif. Android peut arrêter Termux en arrière-plan ; le serveur n'est disponible que tant que Termux et sa session restent actifs.

## Installation

1. Installer [Termux depuis sa page officielle](https://github.com/termux/termux-app#installation) et ouvrir l'application. Le téléphone doit disposer de suffisamment d'espace libre pour compiler et stocker les sauvegardes.
2. Dans Termux, installer les outils puis récupérer le dépôt :

```sh
pkg update && pkg upgrade
pkg install git rust clang make openssh
git clone https://github.com/MEHDImp4/GameSync.git
cd GameSync
cargo build --release --locked -p gamesync-server
```

La compilation peut prendre du temps et ses besoins en mémoire varient selon le téléphone. Si elle échoue, conserver le message d'erreur pour le diagnostic. Utiliser le paquet Rust de Termux, pas rustup.

3. Définir le mot de passe du compte Termux et démarrer SSH :

```sh
passwd
sshd
whoami
```

Noter le nom affiché par `whoami` et l'adresse IPv4 du téléphone dans les réglages Wi-Fi Android. Le serveur SSH Termux utilise le port **8022**. Les deux appareils doivent pouvoir se joindre sur le réseau. Choisir un mot de passe fort et arrêter SSH avec `pkill sshd` lorsqu'il n'est pas utilisé.

4. Lancer le serveur et le laisser ouvert :

```sh
mkdir -p "$HOME/gamesync-data"
export GAMESYNC_DATA="$HOME/gamesync-data"
cd "$HOME/GameSync"
./target/release/gamesync-server
```

Le serveur écoute uniquement sur `127.0.0.1:8787` dans Termux. Pour créer une invitation administrateur, ouvrir une **nouvelle session Termux** et exécuter :

```sh
export GAMESYNC_DATA="$HOME/gamesync-data"
cd "$HOME/GameSync"
./target/release/gamesync-server invite --admin
```

Le code expire après dix minutes ; ne pas le partager.

## Connexion depuis Windows

Installer le client GameSync Windows et utiliser le client OpenSSH de Windows. Dans PowerShell, remplacer `UTILISATEUR` par le résultat de `whoami` et `IP_TELEPHONE` par l'adresse Wi-Fi du téléphone :

```powershell
ssh -p 8022 -L 8787:127.0.0.1:8787 UTILISATEUR@IP_TELEPHONE
```

À la première connexion, vérifier l'empreinte SSH du téléphone avant de l'accepter. Garder cette fenêtre ouverte pendant la synchronisation. Dans GameSync, saisir **http://127.0.0.1:8787**, le nom du PC et le code d'invitation. Cette adresse locale passe dans le tunnel SSH chiffré ; ne pas saisir directement l'adresse HTTP du téléphone. Si le port 8787 est déjà occupé sur Windows, remplacer le premier `8787` dans la commande SSH par un port libre et utiliser ce même port dans GameSync.

Ajouter d'abord un jeu fictif avec un dossier de test et un exécutable associé. Le jeu fermé, synchroniser, puis vérifier que la révision apparaît dans l'historique. Tester la récupération sur un dossier distinct avant de connecter une copie d'une vraie sauvegarde. Laisser la synchronisation automatique désactivée pour ces premiers essais.

## Données et disponibilité

Les données du serveur sont dans `~/gamesync-data`, dans l'espace privé de Termux. La désinstallation de Termux ou l'effacement de ses données les supprime. Arrêter le serveur avant de copier l'intégralité de ce dossier vers un autre support sûr ; ne pas considérer le téléphone comme l'unique copie. Ne pas exposer directement le port 8787 sur le réseau. La mise en veille, les restrictions de batterie ou une interruption réseau peuvent arrêter le service ou le tunnel : relancer `sshd`, le serveur puis la commande SSH si nécessaire.
