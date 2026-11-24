# GameSync

GameSync est un projet de synchronisation auto-hébergée de sauvegardes de jeux entre Windows et Linux.

## État du dépôt

Le workspace Rust contient le modèle de manifeste, la validation des chemins portables, le scan local en lecture seule avec BLAKE3, la décision de synchronisation à trois états et le prédicat de rétention. Les tests utilisent des fichiers temporaires.

Le stockage local conserve les objets par empreinte BLAKE3 et vérifie leur intégrité à la lecture. La capture retourne un manifeste dont les fichiers sont disponibles dans ce stockage. Le stockage doit être situé dans un dossier privé distinct des racines surveillées.

Les révisions sont conservées dans SQLite. Une publication concurrente fondée sur une version dépassée préserve une variante sans déplacer la révision courante. Une ancienne version peut être republiée comme nouvelle révision.

La recette graphique a été exécutée sous Linux et le serveur a été testé avec Docker. La recette native sur Windows et le déploiement sur Unraid restent à vérifier. Sous Linux, le scan et les écritures parcourent les dossiers par descripteurs et refusent les liens symboliques. Les scans automatiques stables réutilisent les empreintes avec l’identité du fichier, la taille et ses horodatages de modification et de changement. Un scan complet reste requis avant une nouvelle décision de transfert et après trente minutes de réutilisation. Sous Windows, le moteur ouvre et retient les dossiers parents sans partager leur suppression et refuse les points de réanalyse. Ces protections attendent une validation native.

Le moteur de restauration préserve les objets locaux, consigne les manifestes d’origine et de destination puis vérifie chaque écriture. Il reprend une application interrompue lorsque les fichiers correspondent encore à leur état d’origine ou à l’état attendu. Une modification indépendante interrompt la reprise. Après un arrêt brutal, le journal identifie le fichier de préparation et le retire uniquement si son contenu est un préfixe vérifié du fichier attendu. Un contenu inattendu est conservé et bloque la reprise. Le client vérifie le processus configuré avant d’appliquer une révision. Un lancement externe peut encore coïncider avec une opération déjà engagée.

Le moteur de synchronisation relie maintenant les opérations locales et l’API. Il persiste la révision de base, les intentions de publication et les applications en attente. Un conflit bloque la synchronisation du jeu en conservant sa variante sur le serveur. Le moteur peut conserver le conflit ouvert ou republier une variante ou une ancienne révision, avec sauvegarde locale préalable. Ces actions sont exposées dans le détail d’un jeu du client desktop.

## Développement

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Le client Rust partagé prend en charge l’appairage, les révisions et les transferts reprenables avec vérification BLAKE3 sur disque. Un test lance le serveur et vérifie les publications concurrentes, la reprise d’un envoi après redémarrage et la récupération après corruption d’un transfert ou d’un téléchargement partiel.

Le tableau de bord présente le stockage et les dernières publications du serveur. Le [contrat OpenAPI](server/openapi.yaml) est également servi sous `/openapi.yaml`.

## Premier essai Windows et Android

Pour un premier test, le serveur peut être compilé dans Termux sur Android et accessible au client Windows par un tunnel SSH. Cette méthode est expérimentale et attend une validation sur appareil réel ; elle ne fournit pas d’application Android graphique. Suivre le [guide Android](docs/android.md), puis la [recette Windows](docs/validation.md), en commençant par des données fictives et en conservant une copie indépendante des sauvegardes.

## Serveur de développement

```sh
cargo run --locked -p gamesync-server
```

Le serveur écoute sur `127.0.0.1:8787`. `GAMESYNC_PORT` et `GAMESYNC_DATA` configurent le port et le dossier de données. `GET /healthz` est public. Les routes de stockage et de révision utilisent le jeton de l’appareil dans l’en-tête `Authorization: Bearer …`. Générer un code à usage unique valable dix minutes avec `cargo run --locked -p gamesync-server -- invite`. Utiliser `invite --admin` pour établir le premier appareil administrateur. Révoquer un appareil avec `cargo run --locked -p gamesync-server -- revoke UUID`. Utiliser le même dossier de données pour le serveur et ces commandes.

| Méthode | Route | Opération |
| --- | --- | --- |
| GET | `/v1/devices` | Liste les appareils, leur rôle et leur dernière activité |
| POST | `/v1/devices/invitations` | Génère un code ordinaire depuis un appareil administrateur |
| DELETE | `/v1/devices/{id}` | Révoque un autre appareil depuis un appareil administrateur |
| POST | `/v1/pair` | Échange `{code, name}` contre `{device, token}` |
| PUT | `/v1/objects/{digest}` | Enregistre un fichier BLAKE3 à partir du corps brut |
| GET | `/v1/objects/{digest}/{size}` | Lit un fichier après vérification de son intégrité |
| GET | `/v1/games` | Liste les jeux enregistrés et les libellés de leurs dossiers |
| PUT | `/v1/games/{game}` | Enregistre l’identité d’un jeu et ses racines, sans chemins locaux |
| GET | `/v1/games/{game}/head` | Renvoie l’identifiant de la révision courante ou `null` |
| POST | `/v1/games/{game}/head` | Publie `{request, base, manifest, resolves?}` et renvoie `{revision, conflict}` |
| GET | `/v1/games/{game}/revisions?before={id}` | Liste jusqu’à 50 révisions précédant le curseur facultatif |
| GET | `/v1/games/{game}/revisions/{id}` | Renvoie le manifeste, l’appareil et la date |

Les envois reprenables acceptent des fichiers jusqu’à 1 Gio, par blocs de 8 Mio. Interroger `GET /v1/uploads/{digest}/{size}` pour obtenir le décalage, transmettre un bloc avec `PUT /v1/uploads/{digest}/{size}/{offset}`, puis vérifier `complete`. Chaque appareil peut avoir un envoi partiel en attente. `DELETE /v1/uploads` l’annule. Le téléchargement accepte `Range: bytes=N-`. Après compilation du serveur, `python3 server/tests/pairing.py` teste l’appairage et la révocation sur une instance temporaire.

Voir le [guide Docker et Unraid](docs/unraid.md) pour le service conteneurisé.

## Client desktop en développement

Depuis `apps/desktop`, exécuter `npm ci` puis `npm run tauri -- dev`. Pour vérifier le frontend seul, lancer `npm run build`.

Dans Paramètres, appairer le client à l’adresse du serveur. Le jeton est conservé par Secret Service sous Linux et par le Gestionnaire d’identifiants sous Windows. Le trousseau doit être disponible et déverrouillé.

Ajouter un jeu avec le dossier de sauvegarde et le chemin de l’exécutable, puis synchroniser manuellement. Sur le second appareil, sélectionner le jeu dans **Jeux sur le serveur** et renseigner les chemins locaux. Un jeu peut associer jusqu’à seize dossiers de sauvegarde. Le serveur conserve les identifiants et libellés des dossiers. L’historique permet de republier une variante ou une version antérieure. Utiliser des dossiers de test pendant le développement.

Dans Paramètres, activer la synchronisation en arrière-plan et régler l’intervalle des scans. Le moteur combine ces scans avec la surveillance des dossiers et réessaie après une erreur. L’activation est volontaire dans la version de développement. Le démarrage avec la session se règle séparément. Fermer la fenêtre laisse le moteur actif. Sur les bureaux Linux où l’icône de zone de notification n’est pas présumée accessible, la fenêtre est minimisée. Utiliser **Quitter GameSync** pour arrêter le processus.

### Découverte des jeux

La bibliothèque analyse Steam au démarrage et permet de répéter l’analyse après une installation. Les bibliothèques Steam secondaires et les préfixes Proton sont reconnus. Sous Windows, l’analyse consulte aussi les métadonnées du lanceur Epic et les clés d’installation GOG.

Pour proposer des dossiers de sauvegarde, indiquer le chemin d’un manifeste YAML local dans **Jeux installés**, ou déposer le fichier sous le nom `ludusavi.yaml` dans le dossier de données du client. Le catalogue [Ludusavi Manifest](https://github.com/mtkennerly/ludusavi-manifest) est publié sous licence MIT. L’intégration actuelle propose les correspondances de type dossier étiquetées `save`. L’utilisateur confirme le dossier et l’exécutable avant tout suivi. Les règles de fichiers individuels, de registre et les motifs non pris en charge requièrent une configuration manuelle. L’application ne redistribue pas le catalogue dans cette version de développement.

Voir le [guide d’installation et de mise à jour](docs/install.md) pour la construction et l’usage des paquets.


### Recette graphique Linux

Le test de bout en bout utilise le vrai serveur et deux instances Tauri avec des dossiers et trousseaux temporaires. Il nécessite une session graphique ou Xvfb, D-Bus et GNOME Keyring. Après le build frontend, exécuter depuis la racine du dépôt.

```sh
cargo build --locked -p gamesync-server
cargo build --locked --manifest-path apps/desktop/src-tauri/Cargo.toml --features custom-protocol,desktop-smoke
python3 apps/desktop/tests/smoke.py
```

Le test couvre l’appairage, le rattachement, l’aller-retour des sauvegardes, la conservation des fichiers pendant une panne serveur, la résolution d’un conflit, la protection pendant que le jeu est ouvert et l’affichage de l’activité. L’instrumentation est compilée uniquement dans les builds de test en mode debug.

Voir aussi [l’architecture et les garanties du protocole](docs/architecture.md).

Voir le [guide de dépannage](docs/troubleshooting.md).

Voir la [checklist de recette Windows et Unraid](docs/validation.md).
