# Recette GameSync

Utiliser deux PC Windows, un hôte Unraid et un dossier de sauvegarde créé pour ce test sur chaque PC. Garder les sauvegardes réelles hors du scénario. Le binaire de test du jeu peut être une copie d’une application qui reste ouverte, avec un nom de fichier distinct sur chaque PC. Les fichiers JavaScript, Rust et Node ne sont pas nécessaires sur les postes de recette.

## Déploiement et installation

- [ ] Déployer le conteneur sur Unraid avec les volumes persistants. Vérifier son état de santé, puis son accès depuis les deux PC via le réseau privé. [Guide Unraid](unraid.md)
- [ ] Installer GameSync sur les deux PC avec le paquet NSIS x64 et vérifier le démarrage depuis le menu Démarrer. Consigner les versions de Windows et de WebView2 pour chaque poste. [Guide d’installation](install.md)
- [ ] Appairer le premier appareil en administrateur, puis utiliser son invitation pour le second. Relancer les deux applications et vérifier que l’appairage est toujours valide.
- [ ] Ajouter un jeu manuel sur le PC A. Créer un fichier de test dans son dossier de sauvegarde, fermer le processus associé et synchroniser. Rattacher le jeu depuis le PC B en sélectionnant son propre dossier.
- [ ] Vérifier l’identité du fichier sur les deux PC avec `Get-FileHash`. Modifier le fichier sur B et vérifier l’arrivée des nouveaux octets sur A.

## Protection des sauvegardes

- [ ] Lancer le processus configuré sur B. Publier un changement depuis A et vérifier que B conserve ses fichiers pendant que le processus est ouvert. Fermer le processus et vérifier la reprise automatique.
- [ ] Arrêter le conteneur. Créer un contenu différent sur chaque PC et vérifier que les deux copies locales restent disponibles. Redémarrer le conteneur et synchroniser les deux appareils.
- [ ] Vérifier que l’historique montre la révision courante et la variante en conflit. Choisir la variante, vérifier sa propagation, puis restaurer une ancienne révision. Vérifier que l’historique permet toujours de récupérer la progression qui précédait la restauration.
- [ ] Recréer le conteneur avec les mêmes volumes. Vérifier les appairages, l’historique, le stockage et la politique de rétention.
- [ ] Révoquer le PC B depuis le PC A. Vérifier que B signale l’échec de synchronisation et conserve ses fichiers.

## Exécution en arrière-plan

- [ ] Activer l’automatisation et le démarrage avec la session. Fermer la fenêtre et vérifier que la synchronisation continue. Ouvrir la fenêtre depuis le menu de la barre système.
- [ ] Redémarrer le PC et vérifier que le client démarre sans intervention. Contrôler l’activité après une modification du fichier de test.
- [ ] Utiliser **Quitter** depuis le menu de la barre système et vérifier que le processus est arrêté.

Consigner les versions et empreintes des installateurs, les heures des opérations et le premier écart observé. Les tests graphiques Linux, les tests de restauration Windows exécutés via Wine et les tests Docker locaux apportent une couverture complémentaire. La recette ci-dessus attend une exécution sur les deux PC et l’hôte Unraid.

Le workflow de build prévoit également une installation NSIS silencieuse sur un runner Windows neuf. Il lance l’exécutable installé et vérifie que le moteur de l’interface initialise la base locale. Cette étape attend sa première exécution sur le runner Windows.
